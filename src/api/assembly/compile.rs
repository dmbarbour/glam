//! The runtime's compile service (`api-runtime-compile`): the compile
//! builtin's compiler reaches the reasoning session weakly, so a served
//! source compiles on the thread that forces it from runtime-side parts, the
//! value factory, the front ends, an invocation counter and the session's
//! diagnostics, and never builds an `Assembler`. Compilation is a pure
//! function, so its diagnostics are reflection effects, on the session's
//! reflection route rather than any machine's output.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use super::{CompilationExecution, FrontEnd, ReasoningSession, ServedSite, record_label};
use crate::api::runtime::admit_runtime_request;
use crate::api::{Diagnostic, Error};
use crate::compiler::{
    CompileCall, CompileContext, CompileDiagnosticEmitter, ImportCompilers, ServedImports,
    SourceCache, SourceCompiler, SourceRecord, SourceRequester, import_failure,
};
use crate::core::{
    Builtin, BuiltinCall, CoreValueFactory, Dict, EvaluationFailure, Key, LazyValue,
    ManagedPromiseRoot, PromisedValue, Value as CoreValue, keys,
};
use crate::diagnostic::{CompilationInvocationId, CompilationTrace, Severity};
use crate::number::Number;
use crate::runtime::RuntimeValueRoot;
use crate::source::{SourceArtifact, SourceIdentity};

impl ReasoningSession {
    /// The value factory of this session's runtime.
    pub(super) fn core_values(&self) -> CoreValueFactory {
        self.runtime.values().core().clone()
    }

    /// The next compilation invocation, one counter for the runtime.
    pub(super) fn next_compilation_invocation(&self) -> CompilationInvocationId {
        let id = self.compile_invocations.fetch_add(1, Ordering::Relaxed);
        assert!(id != u64::MAX, "compilation invocation IDs exhausted");
        CompilationInvocationId::new(id)
    }

    /// Runs `compile` in the macro session every compile shares; it ends
    /// when its front end does.
    pub(super) fn compile_in_execution(
        &self,
        failure: &dyn Fn(String) -> Arc<EvaluationFailure>,
        compile: impl FnOnce(
            Arc<CompilationExecution>,
        ) -> Result<RuntimeValueRoot, Arc<EvaluationFailure>>,
    ) -> Result<RuntimeValueRoot, Arc<EvaluationFailure>> {
        let execution = self.compilation_execution().map_err(|error: Error| {
            failure(format!(
                "could not create its compilation execution: {error}"
            ))
        })?;
        compile(execution)
    }

    /// The compiler for a client's compile, `site` absent, or for one
    /// import site (`compile-yields-an-open-mixin`).
    pub(in crate::api) fn source_compiler(
        self: &Arc<Self>,
        site: Option<ServedSite>,
    ) -> SourceCompiler {
        let session = Arc::downgrade(self);
        SourceCompiler(Arc::new(move |record, call| {
            let subject = match &site {
                Some(site) => format!("local import `{}`", site.import.request),
                None => format!("source `{}`", record_label(&record)),
            };
            let Some(session) = session.upgrade() else {
                return Err(Arc::new(EvaluationFailure::message(format!(
                    "{subject} cannot compile after its runtime was dropped"
                ))));
            };
            let failure = |message: String| {
                Arc::new(EvaluationFailure::message(format!("{subject} {message}")))
            };
            session.compile_in_execution(&failure, |execution| {
                session.compile_served(record, call, site.clone(), execution)
            })
        }))
    }

    /// Writes a request for the source a key names
    /// (`imports-are-source-requests`). The route holds the runtime weakly,
    /// since import values beneath its managed graph keep it.
    pub(in crate::api) fn source_requester(&self) -> SourceRequester {
        let resources = Arc::downgrade(&self.runtime.shared_resources());
        let values = self.core_values();
        Arc::new(move |key| {
            let resources = resources.upgrade().ok_or_else(|| {
                Arc::new(EvaluationFailure::message(
                    "a source cannot be asked for after its runtime was dropped",
                ))
            })?;
            let promises = resources.reflection_snapshot().1.promises().clone();
            let (reply, request) = values.with_boundary_access(|access| {
                let (_, reply, resolver) = promises.allocate(&access);
                let request = Dict::new_sync()
                    .insert(keys::REQUEST.clone(), key.clone_core_with(&access))
                    .insert(keys::REPLY.clone(), resolver);
                (
                    access.root_runtime_value(reply),
                    access.root_runtime_value(CoreValue::Dict(request)),
                )
            });
            admit_runtime_request(&resources, request);
            Ok(reply)
        })
    }

    /// Compiles a served record's source for one application, on the thread
    /// that forces it. Where a compile-time evaluation waited on a reply, the
    /// attempt yields what waits for it and compiles again, with what the
    /// attempt learned (`compile-yields-an-open-mixin`); its diagnostics go
    /// with it.
    fn compile_served(
        self: &Arc<Self>,
        record: SourceRecord,
        call: CompileCall,
        site: Option<ServedSite>,
        execution: Arc<CompilationExecution>,
    ) -> Result<RuntimeValueRoot, Arc<EvaluationFailure>> {
        let values = self.core_values();
        let label = record_label(&record);
        let failure = |message: String, trace: Option<&CompilationTrace>, source| match &site {
            Some(site) => import_failure(
                Some(&values),
                message,
                &site.import.request,
                trace.or(Some(&site.parent)),
                source,
            ),
            None => Arc::new(EvaluationFailure::message(message)),
        };
        let front_end = FrontEnd::for_extension(&record.lang)
            .map_err(|message| failure(format!("`{label}`: {message}"), None, None))?;
        let identity = match &record.scope.location {
            Some(location) => SourceIdentity::keyed(label.clone(), location.key()),
            None => SourceIdentity::script(label.clone(), record.source.clone()),
        };
        let source = Arc::new(SourceArtifact::new(record.source.clone(), identity));
        let namespace = record.scope.namespace.clone();
        let (invocation, replayed) = self.replayed(&call.replay);
        let invocation = invocation.unwrap_or_else(|| self.next_compilation_invocation());
        // A compile forced while its importer runs shares the importer's
        // cache, so that the importer, compiling again, asks for nothing
        // this compile asked for.
        let sources = match site.as_ref().and_then(|site| site.sources.upgrade()) {
            Some(shared) => {
                let mut cached = shared.lock().unwrap_or_else(PoisonError::into_inner);
                for (key, source) in replayed {
                    if !cached.iter().any(|(asked, _)| *asked == key) {
                        cached.push((key, source));
                    }
                }
                drop(cached);
                shared
            }
            None => Arc::new(Mutex::new(replayed)),
        };
        let trace = Arc::new(match &site {
            Some(site) => CompilationTrace::imported(
                invocation,
                &source,
                namespace.clone(),
                site.parent.clone(),
                site.import.request.clone(),
                site.import.extends.clone(),
            ),
            None => CompilationTrace::root(invocation, &source, namespace.clone()),
        });
        let had_errors = Arc::new(AtomicBool::new(false));
        let held = Arc::new(Mutex::new(Vec::new()));
        let compilers = self.import_compilers(trace.clone(), Arc::downgrade(&sources));
        let context =
            CompileContext::from_module_path_with_values(values.clone(), namespace.iter().cloned())
                .with_importer_source(source.clone())
                .with_compilation_trace(trace.clone())
                .with_prior_defs_root(call.base.clone())
                .with_final_defs_root(call.self_value.clone())
                .with_served_imports(ServedImports {
                    location: record.scope.location.clone(),
                    compilers,
                    requester: self.source_requester(),
                    sources: sources.clone(),
                })
                .with_compilation_execution(execution)
                .with_diagnostic_emitter(self.served_diagnostic_emitter(
                    trace.clone(),
                    had_errors.clone(),
                    held.clone(),
                ));
        let definitions = front_end.compile(source.bytes(), &context);
        if let Some(reply) = context.take_wait() {
            let sources = sources
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            return Ok(self.compile_again(call, reply, invocation, sources));
        }
        for diagnostic in std::mem::take(&mut *held.lock().unwrap_or_else(PoisonError::into_inner))
        {
            self.diagnostics.publish_local(diagnostic);
        }
        if had_errors.load(Ordering::Relaxed) {
            return Err(failure(
                format!("`{label}` failed to compile"),
                Some(&trace),
                Some(&source),
            ));
        }
        Ok(definitions)
    }

    /// What earlier attempts of a compile learned, from its `replay`
    /// argument, `{invocation, sources}`: its invocation, and the sources it
    /// asked for, by key. The runtime builds that argument, so it is data
    /// whose outer dicts are evaluated.
    fn replayed(
        &self,
        replay: &RuntimeValueRoot,
    ) -> (
        Option<CompilationInvocationId>,
        Vec<(Key, RuntimeValueRoot)>,
    ) {
        self.core_values().with_boundary_access(|access| {
            replay
                .with_core(&access, |replay| {
                    let CoreValue::Dict(replay) = replay else {
                        return (None, Vec::new());
                    };
                    let invocation = match replay.get(&keys::INVOCATION) {
                        Some(CoreValue::Number(number)) => number
                            .to_u64_if_integer()
                            .filter(|id| *id != 0)
                            .map(CompilationInvocationId::new),
                        _ => None,
                    };
                    let sources = match replay.get(&keys::SOURCES) {
                        Some(CoreValue::Dict(sources)) => sources
                            .iter()
                            .map(|(key, source)| {
                                (
                                    key.clone(),
                                    access.root_runtime_value(access.duplicate_value(source)),
                                )
                            })
                            .collect(),
                        _ => Vec::new(),
                    };
                    (invocation, sources)
                })
                .expect("a compile's replay belongs to its runtime")
        })
    }

    /// `seq reply (compile_source compiler record replay base self)`: waits
    /// for the reply, then compiles again, with what this attempt learned.
    fn compile_again(
        &self,
        call: CompileCall,
        reply: ManagedPromiseRoot,
        invocation: CompilationInvocationId,
        sources: Vec<(Key, RuntimeValueRoot)>,
    ) -> RuntimeValueRoot {
        self.core_values().construct_runtime_value_root(|access| {
            let sources = sources
                .iter()
                .fold(Dict::new_sync(), |dict, (key, source)| {
                    dict.insert(key.clone(), source.clone_core_with(access))
                });
            let replay = Dict::new_sync()
                .insert(
                    keys::INVOCATION.clone(),
                    CoreValue::Number(Number::from_u64(invocation.get())),
                )
                .insert(keys::SOURCES.clone(), CoreValue::Dict(sources));
            let again = CoreValue::Lazy(LazyValue::from_builtin_in(
                access,
                BuiltinCall {
                    builtin: Builtin::CompileSource,
                    arguments: Arc::from([
                        call.compiler.clone_core_with(access),
                        call.record.clone_core_with(access),
                        CoreValue::Dict(replay),
                        call.base.clone_core_with(access),
                        call.self_value.clone_core_with(access),
                    ]),
                },
            ));
            let reply = CoreValue::Promised(PromisedValue::from_root(&reply, access));
            CoreValue::builtin_call_in(access, Builtin::Seq, vec![reply, again])
        })
    }

    /// A compiler for each import site of the compilation `trace` records.
    fn import_compilers(
        self: &Arc<Self>,
        trace: Arc<CompilationTrace>,
        sources: Weak<SourceCache>,
    ) -> ImportCompilers {
        let session = Arc::downgrade(self);
        Arc::new(move |import| {
            let site = ServedSite {
                parent: trace.clone(),
                import: Arc::new(import),
                sources: sources.clone(),
            };
            match session.upgrade() {
                Some(session) => session.source_compiler(Some(site)),
                None => SourceCompiler(Arc::new(|_, _| {
                    Err(Arc::new(EvaluationFailure::message(
                        "an import cannot compile after its runtime was dropped",
                    )))
                })),
            }
        })
    }

    fn served_diagnostic_emitter(
        &self,
        trace: Arc<CompilationTrace>,
        had_errors: Arc<AtomicBool>,
        held: Arc<Mutex<Vec<Diagnostic>>>,
    ) -> CompileDiagnosticEmitter {
        let values = self.runtime.values();
        Arc::new(move |severity, message| {
            if severity == Severity::Error {
                had_errors.store(true, Ordering::Relaxed);
            }
            let diagnostic = Diagnostic::from_compile(values.core(), &trace, severity, &message);
            held.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(diagnostic);
        })
    }
}

#[cfg(test)]
mod tests {
    /// A served compile reaches only the reasoning session's runtime-side
    /// parts, so its code, comments aside, names no assembler, deferred
    /// assembler or source system (`api-runtime-compile`; the lifecycle
    /// review's L3).
    #[test]
    fn the_compile_service_reaches_no_assembler() {
        let code = include_str!("compile.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the module has code before its tests")
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for name in ["Assembler", "deferred(", "SourceSystem"] {
            assert!(!code.contains(name), "the compile service names `{name}`");
        }
    }
}
