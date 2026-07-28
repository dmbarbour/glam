//! Conservative value summaries used by the cached default diagnostic formatter.

use super::super::*;

pub(super) fn apply(context: &EvalContext, arguments: Vec<Value>) -> Result<Value, EvalError> {
    let [header, item_prefix, contexts] = super::exact(arguments, "diagnostic context block")?;
    let header = required_text(context, &header, "diagnostic context block header")?;
    let item_prefix = required_text(
        context,
        &item_prefix,
        "diagnostic context block item prefix",
    )?;
    let contexts = eval_value(context, &contexts)?;
    let frames = match contexts {
        Value::List(contexts) => list_to_value_items(context, &contexts)?,
        Value::Dict(contexts) if contexts.is_empty() => return Ok(Value::binary_from_text("")),
        other => vec![other],
    };
    if frames.is_empty() {
        return Ok(Value::binary_from_text(""));
    }

    let mut block = header;
    for frame in frames {
        block.push_str(&item_prefix);
        block.push_str(&summarize_frame(&frame));
    }
    Ok(Value::binary_from_text(&block))
}

fn summarize_frame(frame: &Value) -> String {
    let Value::Dict(dict) = frame else {
        return frame.diagnostic_kind_name().to_owned();
    };
    let mut entries = dict.iter();
    let Some((tag, payload)) = entries.next() else {
        return frame.diagnostic_kind_name().to_owned();
    };
    if entries.next().is_some() {
        return frame.diagnostic_kind_name().to_owned();
    }

    if tag == &*keys::EVAL {
        return tagged_summary("eval", payload);
    }
    if tag == &*keys::G {
        return g_summary(payload);
    }
    if tag == &*keys::ASM {
        return asm_summary(payload);
    }
    text_key(tag).unwrap_or_else(|| frame.diagnostic_kind_name().to_owned())
}

fn tagged_summary(tag: &str, payload: &Value) -> String {
    immediate_text(payload).map_or_else(|| tag.to_owned(), |text| format!("{tag}: {text}"))
}

fn g_summary(payload: &Value) -> String {
    let Value::Dict(fields) = payload else {
        return "g".to_owned();
    };
    let definition = fields.get(&*keys::DEFINITION).and_then(immediate_text);
    let line = fields.get(&*keys::LINE).and_then(immediate_text);
    match (definition, line) {
        (Some(definition), Some(line)) => {
            format!("g: definition `{definition}` on line {line}")
        }
        (Some(definition), None) => format!("g: definition `{definition}`"),
        (None, Some(line)) => format!("g: line {line}"),
        (None, None) => "g".to_owned(),
    }
}

fn asm_summary(payload: &Value) -> String {
    let Value::Dict(fields) = payload else {
        return "asm".to_owned();
    };
    fields
        .get(&*keys::RESULT)
        .and_then(immediate_text)
        .map_or_else(
            || "asm".to_owned(),
            |result| format!("asm: result `{result}`"),
        )
}

fn immediate_text(value: &Value) -> Option<String> {
    match value {
        Value::Binary(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn required_text(context: &EvalContext, value: &Value, subject: &str) -> Result<String, EvalError> {
    match eval_value(context, value)? {
        Value::Binary(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
        Value::List(list) => list_to_binary_bytes(context, &list, subject)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
        _ => Err(EvalError::new(format!("{subject} must evaluate to text"))),
    }
}

fn text_key(key: &Key) -> Option<String> {
    match key {
        Key::Atom(atom) => text_key(atom.key()),
        Key::Binary(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        Key::Number(_) | Key::AbstractGlobalPath(_) | Key::List(_) | Key::Dict(_) => None,
    }
}
