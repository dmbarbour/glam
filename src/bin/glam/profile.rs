//! The `glam-prof` report.
//!
//! In a `glam-prof` build, an assembly run times its own phases and, when the
//! `GLAM_PROF` environment variable names a path, writes them with the
//! runtime's counters to that path as one JSON object. Ordinary builds keep
//! only the pass-through signatures below, so call sites need no
//! configuration.

use glam::EvaluationRuntime;

/// Runs `work` as the named binary phase, timing it in profiling builds.
pub(super) fn time<R>(phase: &'static str, work: impl FnOnce() -> R) -> R {
    #[cfg(feature = "glam-prof")]
    {
        let start = std::time::Instant::now();
        let result = work();
        enabled::record(phase, start.elapsed());
        result
    }
    #[cfg(not(feature = "glam-prof"))]
    {
        let _ = phase;
        work()
    }
}

/// Writes the report if this is a profiling build and `GLAM_PROF` is set.
pub(super) fn report(runtime: &EvaluationRuntime) {
    #[cfg(feature = "glam-prof")]
    enabled::report(runtime);
    #[cfg(not(feature = "glam-prof"))]
    let _ = runtime;
}

#[cfg(feature = "glam-prof")]
mod enabled {
    use std::sync::{Mutex, OnceLock, PoisonError};
    use std::time::{Duration, Instant};

    use glam::EvaluationRuntime;

    /// Phase durations in the order the phases first ran.
    static PHASES: Mutex<Vec<(&str, Duration)>> = Mutex::new(Vec::new());
    static STARTED: OnceLock<Instant> = OnceLock::new();

    /// Marks the process start; `total_ns` is measured from here.
    pub(super) fn start() {
        let _ = STARTED.set(Instant::now());
    }

    pub(super) fn record(phase: &'static str, elapsed: Duration) {
        let mut phases = PHASES.lock().unwrap_or_else(PoisonError::into_inner);
        match phases.iter_mut().find(|(name, _)| *name == phase) {
            Some((_, total)) => *total += elapsed,
            None => phases.push((phase, elapsed)),
        }
    }

    pub(super) fn report(runtime: &EvaluationRuntime) {
        let Some(path) = std::env::var_os("GLAM_PROF") else {
            return;
        };
        let mut out = String::from("{\"glam_prof\":1,\"phases_ns\":{");
        let phases = PHASES.lock().unwrap_or_else(PoisonError::into_inner);
        for (name, elapsed) in phases.iter() {
            out.push_str(&format!("\"{name}\":{},", elapsed.as_nanos()));
        }
        let total = STARTED
            .get()
            .map_or(0, |started| started.elapsed().as_nanos());
        out.push_str(&format!("\"total\":{total}}},\"runtime\":"));
        runtime.profile().write_json(&mut out);
        out.push_str("}\n");
        if let Err(error) = std::fs::write(&path, out) {
            eprintln!(
                "error: could not write the glam-prof report to `{}`: {error}",
                std::path::Path::new(&path).display()
            );
        }
    }
}

/// Marks the process start in profiling builds.
pub(super) fn start() {
    #[cfg(feature = "glam-prof")]
    enabled::start();
}
