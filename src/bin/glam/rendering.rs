use std::env;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use bytes::Bytes;
use glam::{Assembler, Diagnostic, DiagnosticEvent, DiagnosticSubscriber, Error, Severity, Value};

pub(super) struct DefaultLogger {
    evaluator: Assembler,
    formatter: Value,
    working_directory: PathBuf,
}

impl DefaultLogger {
    const AUTO_INDENT: usize = 4;
    const ANCHOR_INDENT: usize = 2;

    pub(super) fn new(evaluator: Assembler) -> Self {
        let formatter = evaluator.default_diagnostic_formatter();
        Self {
            evaluator,
            formatter,
            working_directory: env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }

    pub(super) fn emit(&self, diagnostic: &Diagnostic) {
        let mut stderr = io::stderr().lock();
        let _ = self.emit_to(diagnostic, &mut stderr);
    }

    fn emit_to(&self, diagnostic: &Diagnostic, writer: &mut impl Write) -> io::Result<()> {
        let terminal = TerminalContext::snapshot();
        let rendered = self
            .format_diagnostic(diagnostic, &terminal)
            .unwrap_or_else(|_| {
                Bytes::from(self.render(diagnostic, diagnostic.message(), &terminal))
            });
        writer.write_all(&rendered)
    }

    fn format_diagnostic(
        &self,
        diagnostic: &Diagnostic,
        terminal: &TerminalContext,
    ) -> Result<Bytes, Error> {
        let values = self.evaluator.values();
        let message = diagnostic.enrich_with(&values, self.viewer_updates(diagnostic, terminal))?;
        let context_lines = self.context_lines(&message, terminal, 0);
        let message =
            Diagnostic::apply_updates(&values, &message, self.context_lines_update(context_lines))?;
        self.format_message(message)
    }

    fn format_message(&self, message: Value) -> Result<Bytes, Error> {
        let values = self.evaluator.values();
        let rendered = values.apply(&self.formatter, [message])?;
        let binary = values.anno_binary(rendered)?;
        let evaluated = self.evaluator.evaluator().eval(&binary)?;
        evaluated
            .as_bytes()?
            .ok_or_else(|| Error::new("diagnostic formatter did not return binary data"))
    }

    fn viewer_updates(&self, diagnostic: &Diagnostic, terminal: &TerminalContext) -> Value {
        let header = format!(
            "{}{}",
            self.location(diagnostic),
            Self::severity_header(diagnostic.severity(), terminal)
        );
        let source = diagnostic.source().and_then(|source| {
            let path = Path::new(source);
            path.is_absolute().then(|| self.display_source(path))
        });
        self.terminal_viewer_updates(terminal, 0, header, self.location(diagnostic), source)
    }

    fn terminal_viewer_updates(
        &self,
        terminal: &TerminalContext,
        base_indent: usize,
        header: String,
        location: String,
        source: Option<String>,
    ) -> Value {
        let values = self.evaluator.values();
        let mut viewer = vec![
            ("kind", values.text("terminal")),
            (
                "columns",
                values.integer(i64::try_from(terminal.columns).unwrap_or(i64::MAX)),
            ),
            ("color", values.text(terminal.color.name())),
            ("header", values.text(header)),
            ("auto_indent", values.integer(Self::AUTO_INDENT as i64)),
            (
                "indent",
                values.text(" ".repeat(base_indent + Self::AUTO_INDENT)),
            ),
            (
                "anchor_indent",
                values.text(" ".repeat(base_indent + Self::ANCHOR_INDENT)),
            ),
            ("location", values.text(location)),
            (
                "context_lines",
                values
                    .list(std::iter::empty())
                    .expect("empty list is local"),
            ),
        ];
        if let Some(term) = &terminal.term {
            viewer.push(("term", values.text(term)));
        }
        if let Some(language) = &terminal.language {
            viewer.push(("lang", values.text(language)));
        }
        if let Some(source) = source {
            viewer.push((
                "source",
                values
                    .record([("file", values.text(source))])
                    .expect("source viewer value is local"),
            ));
        }
        values
            .record([(
                "viewer",
                values.record(viewer).expect("viewer fields are local"),
            )])
            .expect("viewer update is local")
    }

    fn context_lines(
        &self,
        message: &Value,
        terminal: &TerminalContext,
        base_indent: usize,
    ) -> Vec<String> {
        let values = self.evaluator.values();
        let frames = match values
            .access_names(message, ["msg", "context"])
            .and_then(|candidate| {
                values.apply(&values.defined_or_function(), [values.list([])?, candidate])
            })
            .and_then(|contexts| values.anno_array(contexts))
            .and_then(|array| self.evaluator.evaluator().eval(&array))
            .and_then(|array| {
                array
                    .array_items()?
                    .ok_or_else(|| glam::Error::new("context array did not materialize"))
            }) {
            Ok(frames) => frames,
            Err(error) => {
                return vec![
                    format!("{}context:", " ".repeat(base_indent + Self::ANCHOR_INDENT)),
                    format!(
                        "{}msg: <context rendering failed: {error}>",
                        " ".repeat(base_indent + Self::AUTO_INDENT)
                    ),
                ];
            }
        };
        if frames.is_empty() {
            return Vec::new();
        }

        let mut lines = Vec::with_capacity(frames.len() + 1);
        lines.push(format!(
            "{}context:",
            " ".repeat(base_indent + Self::ANCHOR_INDENT)
        ));
        lines.extend(frames.into_iter().map(|frame| {
            self.render_context_frame(&frame, terminal, base_indent + Self::AUTO_INDENT)
        }));
        lines
    }

    fn render_context_frame(
        &self,
        frame: &Value,
        terminal: &TerminalContext,
        frame_indent: usize,
    ) -> String {
        let frame = match self.evaluator.evaluator().eval(frame) {
            Ok(frame) => frame.into_value(),
            Err(error) => {
                return format!(
                    "{}msg: <context rendering failed: {error}>",
                    " ".repeat(frame_indent)
                );
            }
        };
        let message_tag = self.evaluator.values().atom_from_text("msg");
        let is_message = self
            .evaluator
            .reflection()
            .dictionary_items(&frame)
            .is_ok_and(|items| {
                items.into_iter().any(|(tag, _)| {
                    self.evaluator
                        .reflection()
                        .same_representation(&tag, &message_tag)
                        .unwrap_or(false)
                })
            });
        if is_message {
            return self
                .render_context_message(&frame, terminal, frame_indent)
                .unwrap_or_else(|error| {
                    format!(
                        "{}msg: <context rendering failed: {error}>",
                        " ".repeat(frame_indent)
                    )
                });
        }
        format!(
            "{}{}",
            " ".repeat(frame_indent),
            self.summarize_context_frame(&frame)
        )
    }

    fn render_context_message(
        &self,
        message: &Value,
        terminal: &TerminalContext,
        frame_indent: usize,
    ) -> Result<String, Error> {
        let default_header = "msg: ".to_owned();
        let values = self.evaluator.values();
        let message = Diagnostic::apply_updates(
            &values,
            message,
            self.terminal_viewer_updates(
                terminal,
                frame_indent,
                default_header.clone(),
                String::new(),
                None,
            ),
        )?;
        let header = self.context_message_header(&message, terminal);
        let message = if header == default_header {
            message
        } else {
            Diagnostic::apply_updates(&values, &message, self.viewer_header_update(header))?
        };
        let context_lines = self.context_lines(&message, terminal, frame_indent);
        let message =
            Diagnostic::apply_updates(&values, &message, self.context_lines_update(context_lines))?;
        let rendered = self.format_message(message)?;
        let rendered = String::from_utf8_lossy(&rendered);
        let rendered = rendered.strip_suffix('\n').unwrap_or(&rendered);
        Ok(format!("{}{rendered}", " ".repeat(frame_indent)))
    }

    fn context_message_header(&self, message: &Value, terminal: &TerminalContext) -> String {
        let values = self.evaluator.values();
        let Ok(severity) = values
            .access_names(message, ["msg", "severity"])
            .and_then(|severity| self.evaluator.evaluator().eval(&severity))
        else {
            return "msg: ".to_owned();
        };
        let Ok(key) = self.evaluator.reflection().atom_key(severity.as_value()) else {
            return "msg: ".to_owned();
        };
        match diagnostic_text(&self.evaluator, &key).as_deref() {
            Some("info") => Self::severity_header(Severity::Info, terminal),
            Some("warn") => Self::severity_header(Severity::Warning, terminal),
            Some("error") => Self::severity_header(Severity::Error, terminal),
            _ => "msg: ".to_owned(),
        }
    }

    fn viewer_header_update(&self, header: String) -> Value {
        let values = self.evaluator.values();
        values
            .record([(
                "viewer",
                values
                    .record([("header", values.text(header))])
                    .expect("viewer header is local"),
            )])
            .expect("viewer update is local")
    }

    fn context_lines_update(&self, lines: Vec<String>) -> Value {
        let values = self.evaluator.values();
        let lines = values
            .list(lines.into_iter().map(|line| values.text(line)))
            .expect("context lines are local");
        values
            .record([(
                "viewer",
                values
                    .record([("context_lines", lines)])
                    .expect("context-line viewer field is local"),
            )])
            .expect("viewer update is local")
    }

    fn summarize_context_frame(&self, frame: &Value) -> String {
        let reflection = self.evaluator.reflection();
        let Ok(entries) = reflection.dictionary_items(frame) else {
            return diagnostic_value_kind(&self.evaluator, frame).to_owned();
        };
        let [(tag, payload)] = entries.as_slice() else {
            return diagnostic_value_kind(&self.evaluator, frame).to_owned();
        };

        let values = self.evaluator.values();
        if reflection
            .same_representation(tag, &values.atom_from_text("eval"))
            .unwrap_or(false)
        {
            return self.eval_context_summary(payload);
        }
        if reflection
            .same_representation(tag, &values.atom_from_text("g"))
            .unwrap_or(false)
        {
            return self.g_context_summary(payload);
        }
        if reflection
            .same_representation(tag, &values.atom_from_text("import"))
            .unwrap_or(false)
        {
            return self.import_context_summary(payload);
        }
        if reflection
            .same_representation(tag, &values.atom_from_text("asm"))
            .unwrap_or(false)
        {
            return self.asm_context_summary(payload);
        }
        if reflection
            .same_representation(tag, &values.atom_from_text("conf"))
            .unwrap_or(false)
        {
            return self.conf_context_summary(payload);
        }
        if reflection
            .same_representation(tag, &values.atom_from_text("task"))
            .unwrap_or(false)
        {
            return self.task_context_summary(payload);
        }
        if reflection
            .same_representation(tag, &values.atom_from_text("runtime"))
            .unwrap_or(false)
        {
            return self.runtime_context_summary(payload);
        }
        self.context_tag_text(tag)
            .unwrap_or_else(|| diagnostic_value_kind(&self.evaluator, frame).to_owned())
    }

    fn eval_context_summary(&self, payload: &Value) -> String {
        let operation = self
            .context_field_tag_text(payload, &["op"])
            .map(|operation| operation.replace('_', " "));
        let path = self.context_field_text(payload, &["args", "path"]);
        match (operation, path) {
            (Some(operation), Some(path)) => format!("eval: {operation} `{path}`"),
            (Some(operation), None) => format!("eval: {operation}"),
            (None, Some(path)) => format!("eval: path `{path}`"),
            (None, None) => "eval".to_owned(),
        }
    }

    fn g_context_summary(&self, payload: &Value) -> String {
        let definition = self.context_field_text(payload, &["definition"]);
        let line = self.context_field_text(payload, &["line"]);
        match (definition, line) {
            (Some(definition), Some(line)) => {
                format!("g: definition `{definition}` on line {line}")
            }
            (Some(definition), None) => format!("g: definition `{definition}`"),
            (None, Some(line)) => format!("g: line {line}"),
            (None, None) => "g".to_owned(),
        }
    }

    fn import_context_summary(&self, payload: &Value) -> String {
        self.context_field_text(payload, &["request", "file"])
            .map_or_else(
                || "import".to_owned(),
                |request| format!("import: request `{request}`"),
            )
    }

    fn asm_context_summary(&self, payload: &Value) -> String {
        self.context_field_text(payload, &["result"]).map_or_else(
            || "asm".to_owned(),
            |result| format!("asm: result `{result}`"),
        )
    }

    fn conf_context_summary(&self, payload: &Value) -> String {
        self.context_field_text(payload, &["entry"]).map_or_else(
            || "conf".to_owned(),
            |entry| format!("conf: entry `{entry}`"),
        )
    }

    fn task_context_summary(&self, payload: &Value) -> String {
        let operation = self.context_field_tag_text(payload, &["operation"]);
        let id = self.context_field_text(payload, &["id"]);
        match (operation, id) {
            (Some(operation), Some(id)) => format!("task: {operation} task {id}"),
            (Some(operation), None) => format!("task: {operation}"),
            (None, Some(id)) => format!("task: task {id}"),
            (None, None) => "task".to_owned(),
        }
    }

    fn runtime_context_summary(&self, payload: &Value) -> String {
        let operation = self
            .context_field_tag_text(payload, &["op"])
            .map(|operation| operation.replace('_', " "));
        let work = self.context_field_text(payload, &["args", "work"]);
        let session = self.context_field_text(payload, &["args", "session"]);
        let task = self.context_field_text(payload, &["args", "task"]);
        let delivery = self.context_field_text(payload, &["args", "delivery"]);
        let endpoint = self.context_field_text(payload, &["args", "endpoint"]);
        let kind = self
            .context_field_tag_text(payload, &["args", "kind"])
            .map(|kind| kind.replace('_', " "));

        let mut details = Vec::new();
        if let Some(work) = work {
            details.push(format!("work {work}"));
        }
        if let Some(session) = session {
            details.push(format!("session {session}"));
        }
        if let Some(task) = task {
            details.push(format!("task {task}"));
        }
        if let Some(delivery) = delivery {
            details.push(format!("delivery {delivery}"));
        }
        if let Some(endpoint) = endpoint {
            details.push(format!("endpoint {endpoint}"));
        }
        if let Some(kind) = kind {
            details.push(kind);
        }

        let operation = operation.unwrap_or_else(|| "event".to_owned());
        if details.is_empty() {
            format!("runtime: {operation}")
        } else {
            format!("runtime: {operation} ({})", details.join(", "))
        }
    }

    fn context_field_text(&self, value: &Value, path: &[&str]) -> Option<String> {
        self.evaluator
            .values()
            .access_names(value, path.iter().copied())
            .ok()
            .and_then(|value| diagnostic_text(&self.evaluator, &value))
    }

    fn context_field_tag_text(&self, value: &Value, path: &[&str]) -> Option<String> {
        self.evaluator
            .values()
            .access_names(value, path.iter().copied())
            .ok()
            .and_then(|value| self.context_tag_text(&value))
    }

    fn context_tag_text(&self, tag: &Value) -> Option<String> {
        diagnostic_text(&self.evaluator, tag).or_else(|| {
            self.evaluator
                .reflection()
                .atom_key(tag)
                .ok()
                .and_then(|key| diagnostic_text(&self.evaluator, &key))
        })
    }

    fn severity_header(severity: Severity, terminal: &TerminalContext) -> String {
        let label = severity.to_string();
        format!("{}: ", terminal.color.paint(severity, &label))
    }

    fn render(&self, diagnostic: &Diagnostic, text: &str, terminal: &TerminalContext) -> String {
        let severity = diagnostic.severity().to_string();
        let severity = terminal.color.paint(diagnostic.severity(), &severity);
        let mut rendered = format!("{}{severity}: ", self.location(diagnostic));
        let mut lines = text.split('\n');
        rendered.push_str(lines.next().unwrap_or_default());
        for line in lines {
            rendered.push('\n');
            if !line.is_empty() {
                rendered.push_str(&" ".repeat(Self::AUTO_INDENT));
                rendered.push_str(line);
            }
        }
        for line in self.context_lines(diagnostic.emission(), terminal, 0) {
            rendered.push('\n');
            rendered.push_str(&line);
        }
        rendered.push('\n');
        rendered
    }

    fn location(&self, diagnostic: &Diagnostic) -> String {
        match (diagnostic.source(), diagnostic.line()) {
            (Some(source), Some(line)) => {
                format!("{}:{line}: ", self.display_source(Path::new(source)))
            }
            (Some(source), None) => format!("{}: ", self.display_source(Path::new(source))),
            (None, Some(line)) => format!("line {line}: "),
            (None, None) => String::new(),
        }
    }

    fn display_source(&self, source: &Path) -> String {
        source
            .strip_prefix(&self.working_directory)
            .unwrap_or(source)
            .display()
            .to_string()
    }
}

fn diagnostic_text(assembler: &Assembler, value: &Value) -> Option<String> {
    let value = assembler.evaluator().eval(value).ok()?;
    value
        .as_bytes()
        .ok()
        .flatten()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .or_else(|| value.number_text().ok().flatten())
}

fn diagnostic_value_kind(assembler: &Assembler, value: &Value) -> &'static str {
    let values = assembler.values();
    if assembler
        .reflection()
        .same_representation(value, &values.abstract_global_path(["builtin", "unit"]))
        .unwrap_or(false)
    {
        return "Unit";
    }
    match assembler.reflection().kind(value) {
        Err(_) => "Foreign",
        Ok(glam::ValueKind::Atom) => "Atom",
        Ok(glam::ValueKind::Number) => "Number",
        Ok(glam::ValueKind::Binary) => "Binary",
        Ok(glam::ValueKind::List) => "List",
        Ok(glam::ValueKind::Dict) => {
            if assembler
                .reflection()
                .dictionary_items(value)
                .is_ok_and(|items| items.is_empty())
            {
                "Undefined"
            } else {
                "Dict"
            }
        }
        Ok(glam::ValueKind::Function) => "Function",
        Ok(glam::ValueKind::Net) => "Net",
        Ok(glam::ValueKind::Lazy) => "Lazy",
        Ok(glam::ValueKind::Sealed) => "Sealed",
        Ok(glam::ValueKind::Opaque) => "Opaque",
        Ok(_) => "Value",
    }
}

impl DiagnosticSubscriber for DefaultLogger {
    fn receive(&self, event: DiagnosticEvent) {
        DefaultLogger::emit(self, &event);
    }
}

struct TerminalContext {
    columns: usize,
    color: TerminalColor,
    term: Option<String>,
    language: Option<String>,
}

impl TerminalContext {
    fn snapshot() -> Self {
        let term = env::var("TERM").ok();
        let color = TerminalColor::detect(term.as_deref());
        Self {
            columns: env::var("COLUMNS")
                .ok()
                .and_then(|columns| columns.parse().ok())
                .filter(|columns| *columns > 0)
                .unwrap_or(80),
            color,
            term,
            language: ["LC_ALL", "LC_MESSAGES", "LANG"]
                .into_iter()
                .find_map(|name| env::var(name).ok().filter(|value| !value.is_empty())),
        }
    }
}

#[derive(Clone, Copy)]
enum TerminalColor {
    None,
    Ansi16,
    Ansi256,
    TrueColor,
}

impl TerminalColor {
    fn detect(term: Option<&str>) -> Self {
        if !io::stderr().is_terminal() || env::var_os("NO_COLOR").is_some() || term == Some("dumb")
        {
            return Self::None;
        }
        if env::var("COLORTERM").is_ok_and(|value| {
            value.eq_ignore_ascii_case("truecolor") || value.eq_ignore_ascii_case("24bit")
        }) {
            Self::TrueColor
        } else if term.is_some_and(|term| term.contains("256color")) {
            Self::Ansi256
        } else {
            Self::Ansi16
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Ansi16 => "ansi16",
            Self::Ansi256 => "ansi256",
            Self::TrueColor => "truecolor",
        }
    }

    fn paint(self, severity: Severity, text: &str) -> String {
        let code = match (self, severity) {
            (Self::None, _) => return text.to_owned(),
            (_, Severity::Info) => 36,
            (_, Severity::Warning) => 33,
            (_, Severity::Error) => 31,
        };
        format!("\x1b[{code}m{text}\x1b[0m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_default_logger_owner_inventory(logger: &DefaultLogger) {
        let DefaultLogger {
            evaluator: _,
            formatter,
            working_directory: _,
        } = logger;
        let _: &Value = formatter;
    }

    #[test]
    fn default_logger_owner_inventory_is_compile_exhaustive() {
        let _: fn(&DefaultLogger) = assert_default_logger_owner_inventory;
    }

    struct CollectingWriter {
        heap: glam_gc::Heap,
        bytes: Vec<u8>,
    }

    impl Write for CollectingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.heap.collect_full().map_err(io::Error::other)?;
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    trait TestValueFacade {
        fn get(&self, root: &Value, path: &str) -> Result<Value, Error>;
        fn get_evaluated(&self, root: &Value, path: &str) -> Result<glam::EvaluatedValue, Error>;
    }

    impl TestValueFacade for Assembler {
        fn get(&self, root: &Value, path: &str) -> Result<Value, Error> {
            self.get_evaluated(root, path)
                .map(glam::EvaluatedValue::into_value)
        }

        fn get_evaluated(&self, root: &Value, path: &str) -> Result<glam::EvaluatedValue, Error> {
            let value = self.values().access_names(root, path.split('.'))?;
            self.evaluator().eval(&value)
        }
    }

    fn record<I, S>(values: &glam::Values, entries: I) -> Value
    where
        I: IntoIterator<Item = (S, Value)>,
        S: AsRef<str>,
    {
        values.record(entries).expect("test record should be local")
    }

    fn list(values: &glam::Values, items: impl IntoIterator<Item = Value>) -> Value {
        values.list(items).expect("test list should be local")
    }

    #[test]
    fn glam_default_formatter_renders_location_severity_and_continuation_lines() {
        let evaluator = Assembler::default();
        let values = evaluator.values();
        let logger = DefaultLogger {
            formatter: evaluator.default_diagnostic_formatter(),
            evaluator,
            working_directory: PathBuf::from("/work"),
        };
        let diagnostic = Diagnostic::new(&values, Severity::Warning, "first\nsecond\n\nfourth")
            .with_source_location(&values, "/work/src/test.g", 4)
            .expect("source location should use the diagnostic runtime");
        let terminal = TerminalContext {
            columns: 80,
            color: TerminalColor::None,
            term: None,
            language: None,
        };
        let rendered = logger
            .format_diagnostic(&diagnostic, &terminal)
            .expect("the closed Glam formatter should return bytes");

        assert_eq!(
            rendered,
            Bytes::from_static(b"src/test.g:4: warning: first\n    second\n    \n    fourth\n")
        );
    }

    #[test]
    fn diagnostic_rendering_invokes_writer_without_mutator() {
        let evaluator = Assembler::default();
        let values = evaluator.values();
        let logger = DefaultLogger {
            formatter: evaluator.default_diagnostic_formatter(),
            evaluator,
            working_directory: PathBuf::from("/work"),
        };
        let diagnostic = Diagnostic::new(&values, Severity::Info, "rendered");
        let mut writer = CollectingWriter {
            heap: glam_gc::Heap::new_with_policy(glam_gc::CollectionPolicy::NoAuto),
            bytes: Vec::new(),
        };

        logger
            .emit_to(&diagnostic, &mut writer)
            .expect("terminal writing should run without any inherited mutator");

        assert_eq!(writer.bytes, b"info: rendered\n");
    }

    #[test]
    fn glam_default_formatter_renders_recognized_context_frames() {
        let evaluator = Assembler::default();
        let values = evaluator.values();
        let logger = DefaultLogger {
            formatter: evaluator.default_diagnostic_formatter(),
            evaluator,
            working_directory: PathBuf::from("/work"),
        };
        let diagnostic = Diagnostic::from_emission(
            &values,
            Severity::Error,
            record(
                &values,
                [(
                    "msg",
                    record(
                        &values,
                        [
                            ("text", values.text("broken\nmore detail")),
                            (
                                "context",
                                list(
                                    &values,
                                    [
                                        record(
                                            &values,
                                            [(
                                                "eval",
                                                record(
                                                    &values,
                                                    [(
                                                        "op",
                                                        values.atom_from_text("binary_extraction"),
                                                    )],
                                                ),
                                            )],
                                        ),
                                        record(
                                            &values,
                                            [(
                                                "g",
                                                record(
                                                    &values,
                                                    [
                                                        ("definition", values.text("result")),
                                                        ("line", values.integer(7)),
                                                    ],
                                                ),
                                            )],
                                        ),
                                        record(
                                            &values,
                                            [(
                                                "import",
                                                record(
                                                    &values,
                                                    [(
                                                        "request",
                                                        record(
                                                            &values,
                                                            [("file", values.text("child.g"))],
                                                        ),
                                                    )],
                                                ),
                                            )],
                                        ),
                                        record(
                                            &values,
                                            [(
                                                "asm",
                                                record(
                                                    &values,
                                                    [("result", values.text("asm.result"))],
                                                ),
                                            )],
                                        ),
                                        record(
                                            &values,
                                            [(
                                                "eval",
                                                record(
                                                    &values,
                                                    [
                                                        (
                                                            "op",
                                                            values.atom_from_text("path_lookup"),
                                                        ),
                                                        (
                                                            "args",
                                                            record(
                                                                &values,
                                                                [("path", values.text("conf.env"))],
                                                            ),
                                                        ),
                                                    ],
                                                ),
                                            )],
                                        ),
                                        record(
                                            &values,
                                            [(
                                                "conf",
                                                record(&values, [("entry", values.text("log"))]),
                                            )],
                                        ),
                                        record(
                                            &values,
                                            [(
                                                "task",
                                                record(
                                                    &values,
                                                    [
                                                        (
                                                            "operation",
                                                            values.atom_from_text("join"),
                                                        ),
                                                        ("id", values.integer(12)),
                                                    ],
                                                ),
                                            )],
                                        ),
                                        record(
                                            &values,
                                            [(
                                                "runtime",
                                                record(
                                                    &values,
                                                    [
                                                        (
                                                            "op",
                                                            values
                                                                .atom_from_text("delivery_failure"),
                                                        ),
                                                        (
                                                            "args",
                                                            record(
                                                                &values,
                                                                [
                                                                    (
                                                                        "delivery",
                                                                        values.integer(13),
                                                                    ),
                                                                    ("endpoint", values.integer(4)),
                                                                    (
                                                                        "kind",
                                                                        values.atom_from_text(
                                                                            "adapter",
                                                                        ),
                                                                    ),
                                                                ],
                                                            ),
                                                        ),
                                                    ],
                                                ),
                                            )],
                                        ),
                                    ],
                                ),
                            ),
                        ],
                    ),
                )],
            ),
        )
        .expect("diagnostic emission should use the formatter runtime");
        let terminal = TerminalContext {
            columns: 80,
            color: TerminalColor::None,
            term: None,
            language: None,
        };
        let rendered = logger
            .format_diagnostic(&diagnostic, &terminal)
            .expect("the closed Glam formatter should render contexts");

        assert_eq!(
            rendered,
            Bytes::from_static(
                b"error: broken\n    more detail\n  context:\n    eval: binary extraction\n    g: definition `result` on line 7\n    import: request `child.g`\n    asm: result `asm.result`\n    eval: path lookup `conf.env`\n    conf: entry `log`\n    task: join task 12\n    runtime: delivery failure (delivery 13, endpoint 4, adapter)\n"
            )
        );
    }

    #[test]
    fn glam_default_formatter_recursively_renders_context_messages() {
        let evaluator = Assembler::default();
        let values = evaluator.values();
        let logger = DefaultLogger {
            formatter: evaluator.default_diagnostic_formatter(),
            evaluator,
            working_directory: PathBuf::from("/work"),
        };
        let diagnostic = Diagnostic::from_emission(
            &values,
            Severity::Error,
            record(&values, [(
                "msg",
                record(&values, [
                    ("text", values.text("outer failure")),
                    (
                        "context",
                        list(&values, [
                            record(&values, [(
                                "msg",
                                record(&values, [("text", values.text("unclassified context"))]),
                            )]),
                            record(&values, [(
                                "msg",
                                record(&values, [
                                    ("text", values.text("nested context\nmore detail")),
                                    ("severity", values.atom_from_text("info")),
                                    (
                                        "context",
                                        list(&values, [record(&values, [(
                                            "eval",
                                            record(&values, [(
                                                "op",
                                                values.atom_from_text("list_index"),
                                            )]),
                                        )])]),
                                    ),
                                ]),
                            )]),
                        ]),
                    ),
                ]),
            )]),
        )
        .expect("diagnostic emission should use the formatter runtime");
        let terminal = TerminalContext {
            columns: 80,
            color: TerminalColor::None,
            term: Some("xterm-256color".to_owned()),
            language: Some("en_US.UTF-8".to_owned()),
        };

        let rendered = logger
            .format_diagnostic(&diagnostic, &terminal)
            .expect("context messages should use the recursive diagnostic view");

        assert_eq!(
            rendered,
            Bytes::from_static(
                b"error: outer failure\n  context:\n    msg: unclassified context\n    info: nested context\n        more detail\n      context:\n        eval: list index\n"
            )
        );
    }

    #[test]
    fn glam_default_formatter_recognizes_full_objects_as_context_messages() {
        let evaluator = Assembler::default();
        let values = evaluator.values();
        let module = evaluator
            .module(["context_fixture"])
            .script(
                "g",
                concat!(
                    "language g0\n",
                    "object frame with\n",
                    "  msg = {text:viewer.term, severity:'info}\n",
                ),
            )
            .build()
            .expect("context object fixture should compile");
        let frame = evaluator
            .get(module.value(), "frame")
            .expect("context object should be available");
        assert!(
            evaluator.get(&frame, "spec").is_ok(),
            "fixture must retain its object interface"
        );
        let logger = DefaultLogger {
            formatter: evaluator.default_diagnostic_formatter(),
            evaluator,
            working_directory: PathBuf::from("/work"),
        };
        let diagnostic = Diagnostic::from_emission(
            &values,
            Severity::Error,
            record(
                &values,
                [(
                    "msg",
                    record(
                        &values,
                        [
                            ("text", values.text("outer failure")),
                            ("context", list(&values, [frame])),
                        ],
                    ),
                )],
            ),
        )
        .expect("diagnostic emission should use the formatter runtime");
        let terminal = TerminalContext {
            columns: 80,
            color: TerminalColor::None,
            term: Some("object terminal context".to_owned()),
            language: None,
        };

        let rendered = logger
            .format_diagnostic(&diagnostic, &terminal)
            .expect("a context object should retain its view behavior");

        assert_eq!(
            rendered,
            Bytes::from_static(
                b"error: outer failure\n  context:\n    info: object terminal context\n"
            )
        );
    }

    #[test]
    fn failed_context_message_rendering_does_not_hide_the_primary_diagnostic() {
        let evaluator = Assembler::default();
        let values = evaluator.values();
        let logger = DefaultLogger {
            formatter: evaluator.default_diagnostic_formatter(),
            evaluator,
            working_directory: PathBuf::from("/work"),
        };
        let diagnostic = Diagnostic::from_emission(
            &values,
            Severity::Error,
            record(
                &values,
                [(
                    "msg",
                    record(
                        &values,
                        [
                            ("text", values.text("outer failure")),
                            (
                                "context",
                                list(
                                    &values,
                                    [record(
                                        &values,
                                        [("msg", record(&values, [("text", values.integer(42))]))],
                                    )],
                                ),
                            ),
                        ],
                    ),
                )],
            ),
        )
        .expect("diagnostic emission should use the formatter runtime");
        let terminal = TerminalContext {
            columns: 80,
            color: TerminalColor::None,
            term: None,
            language: None,
        };

        let rendered = logger
            .format_diagnostic(&diagnostic, &terminal)
            .expect("a malformed context message should have a local fallback");
        let rendered = String::from_utf8_lossy(&rendered);

        assert!(rendered.starts_with("error: outer failure\n  context:\n"));
        assert!(rendered.contains("    msg: <context rendering failed:"));
    }

    #[test]
    fn glam_default_formatter_summarizes_unrecognized_context_frames() {
        let evaluator = Assembler::default();
        let values = evaluator.values();
        let logger = DefaultLogger {
            formatter: evaluator.default_diagnostic_formatter(),
            evaluator,
            working_directory: PathBuf::from("/work"),
        };
        let diagnostic = Diagnostic::from_emission(
            &values,
            Severity::Warning,
            record(
                &values,
                [(
                    "msg",
                    record(
                        &values,
                        [
                            ("text", values.text("careful")),
                            (
                                "context",
                                list(
                                    &values,
                                    [
                                        record(&values, [("custom", values.integer(42))]),
                                        record(
                                            &values,
                                            [
                                                ("left", values.integer(1)),
                                                ("right", values.integer(2)),
                                            ],
                                        ),
                                        values.integer(7),
                                    ],
                                ),
                            ),
                        ],
                    ),
                )],
            ),
        )
        .expect("diagnostic emission should use the formatter runtime");
        let terminal = TerminalContext {
            columns: 80,
            color: TerminalColor::None,
            term: None,
            language: None,
        };
        let rendered = logger
            .format_diagnostic(&diagnostic, &terminal)
            .expect("the closed Glam formatter should summarize unknown contexts");

        assert_eq!(
            rendered,
            Bytes::from_static(b"warning: careful\n  context:\n    custom\n    Dict\n    Number\n")
        );
    }

    #[test]
    fn glam_default_formatter_applies_terminal_color_policy() {
        let evaluator = Assembler::default();
        let values = evaluator.values();
        let logger = DefaultLogger {
            formatter: evaluator.default_diagnostic_formatter(),
            evaluator,
            working_directory: PathBuf::from("/work"),
        };
        let diagnostic = Diagnostic::new(&values, Severity::Error, "broken");
        let terminal = TerminalContext {
            columns: 80,
            color: TerminalColor::Ansi256,
            term: None,
            language: None,
        };
        let rendered = logger
            .format_diagnostic(&diagnostic, &terminal)
            .expect("the closed Glam formatter should return colored bytes");

        assert_eq!(
            rendered,
            Bytes::from_static(b"\x1b[31merror\x1b[0m: broken\n")
        );
    }

    #[test]
    fn terminal_viewer_context_is_an_independent_diagnostic_mixin() {
        let logger = DefaultLogger::new(Assembler::default());
        let diagnostic = Diagnostic::new(&logger.evaluator.values(), Severity::Info, "hello");
        let terminal = TerminalContext {
            columns: 100,
            color: TerminalColor::Ansi256,
            term: Some("xterm-256color".to_owned()),
            language: Some("en_US.UTF-8".to_owned()),
        };
        let values = logger.evaluator.values();
        let enriched = diagnostic
            .enrich_with(&values, logger.viewer_updates(&diagnostic, &terminal))
            .expect("terminal viewer metadata should mix into a diagnostic");

        assert_eq!(
            logger
                .evaluator
                .get_evaluated(&enriched, "viewer.auto_indent")
                .expect("viewer should declare automatic indentation")
                .as_i64()
                .unwrap(),
            Some(4)
        );
        assert_eq!(
            logger
                .evaluator
                .get_evaluated(&enriched, "viewer.header")
                .expect("viewer should materialize the complete message header")
                .as_bytes()
                .unwrap()
                .as_deref(),
            Some(b"\x1b[36minfo\x1b[0m: ".as_slice())
        );
        assert_eq!(
            logger
                .evaluator
                .get_evaluated(&enriched, "viewer.anchor_indent")
                .expect("viewer should expose its section anchor indentation")
                .as_bytes()
                .unwrap()
                .as_deref(),
            Some(b"  ".as_slice())
        );
        assert_eq!(
            logger
                .evaluator
                .get_evaluated(&enriched, "viewer.term")
                .expect("viewer should declare its terminal")
                .as_bytes()
                .unwrap()
                .as_deref(),
            Some(b"xterm-256color".as_slice())
        );
        let viewer = logger
            .evaluator
            .get_evaluated(diagnostic.emission(), "viewer")
            .expect("the raw diagnostic should expose undefined viewer metadata");
        assert!(
            logger
                .evaluator
                .reflection()
                .dictionary_items(viewer.as_value())
                .is_ok_and(|items| items.is_empty())
        );
    }
}
