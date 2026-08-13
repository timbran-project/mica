use crate::{
    BuiltinContext, BuiltinRegistry, BuiltinResultKind, RuntimeError, builtin_char_list_arg,
    builtin_string_arg, builtin_usize_arg, invalid_builtin_call, option_builtin_result,
    raised_builtin_error, result_builtin_result,
};
use mica_var::{Symbol, Value, ValueKind};
use mica_vm::TypeContract;

pub(crate) fn install(registry: BuiltinRegistry) -> BuiltinRegistry {
    registry
        .with_builtin(
            "string_len",
            BuiltinResultKind::Exact(ValueKind::Int),
            string_len_builtin,
        )
        .with_builtin(
            "string_chars",
            BuiltinResultKind::Exact(ValueKind::List),
            string_chars_builtin,
        )
        .with_builtin(
            "string_slice",
            BuiltinResultKind::Exact(ValueKind::String),
            string_slice_builtin,
        )
        .with_builtin(
            "string_from_chars",
            BuiltinResultKind::Exact(ValueKind::String),
            string_from_chars_builtin,
        )
        .with_builtin(
            "string_concat",
            BuiltinResultKind::Exact(ValueKind::String),
            string_concat_builtin,
        )
        .with_builtin(
            "string_join",
            BuiltinResultKind::Exact(ValueKind::String),
            string_join_builtin,
        )
        .with_builtin(
            "url_encode_component",
            BuiltinResultKind::Exact(ValueKind::String),
            url_encode_component_builtin,
        )
        .with_builtin(
            "url_decode_component",
            BuiltinResultKind::Exact(ValueKind::String),
            url_decode_component_builtin,
        )
        .with_builtin(
            "sort",
            BuiltinResultKind::Exact(ValueKind::List),
            sort_builtin,
        )
        .with_builtin(
            "words",
            BuiltinResultKind::Exact(ValueKind::List),
            words_builtin,
        )
        .with_builtin(
            "string_starts_with",
            BuiltinResultKind::Exact(ValueKind::Bool),
            string_starts_with_builtin,
        )
        .with_builtin(
            "string_contains",
            BuiltinResultKind::Exact(ValueKind::Bool),
            string_contains_builtin,
        )
        .with_builtin(
            "string_equal_fold",
            BuiltinResultKind::Exact(ValueKind::Bool),
            string_equal_fold_builtin,
        )
        .with_builtin(
            "edit_distance",
            BuiltinResultKind::Exact(ValueKind::Int),
            edit_distance_builtin,
        )
        .with_builtin(
            "parse_ordinal",
            result_builtin_result(TypeContract::Kind(ValueKind::Int)),
            parse_ordinal_builtin,
        )
        .with_builtin(
            "lower",
            BuiltinResultKind::Exact(ValueKind::String),
            lower_builtin,
        )
        .with_builtin(
            "os_getenv",
            option_builtin_result(TypeContract::Kind(ValueKind::String)),
            os_getenv_builtin,
        )
}

fn string_len_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call(
            "string_len",
            "expected string_len(text)",
        ));
    }
    let value = builtin_string_arg("string_len", args, 0)?;
    Value::int(value.chars().count() as i64)
        .map_err(|_| invalid_builtin_call("string_len", "string length is out of range"))
}

fn string_chars_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call(
            "string_chars",
            "expected string_chars(text)",
        ));
    }
    let value = builtin_string_arg("string_chars", args, 0)?;
    Ok(Value::list(
        value.chars().map(|ch| Value::string(ch.to_string())),
    ))
}

fn string_slice_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 3 {
        return Err(invalid_builtin_call(
            "string_slice",
            "expected string_slice(text, start, end)",
        ));
    }
    let value = builtin_string_arg("string_slice", args, 0)?;
    let start = builtin_usize_arg("string_slice", args, 1)?;
    let end = builtin_usize_arg("string_slice", args, 2)?;
    let char_len = value.chars().count();
    if start > end || end > char_len {
        return Err(raised_builtin_error(
            "E_INDEX",
            format!(
                "string_slice bounds {start}..{end} are invalid for a string of length {char_len}"
            ),
            Some(Value::list(args.iter().cloned())),
        ));
    }
    Ok(Value::string(string_slice_chars(&value, start, end)))
}

fn string_from_chars_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call(
            "string_from_chars",
            "expected string_from_chars(chars)",
        ));
    }
    let chars = builtin_char_list_arg("string_from_chars", args, 0)?;
    Ok(Value::string(chars.into_iter().collect::<String>()))
}

fn string_concat_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    let mut out = String::new();
    for (index, value) in args.iter().enumerate() {
        let Some(()) = value.with_str(|value| out.push_str(value)) else {
            return Err(invalid_builtin_call(
                "string_concat",
                format!("argument {} is not a string", index + 1),
            ));
        };
    }
    Ok(Value::string(out))
}

fn string_join_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 2 {
        return Err(invalid_builtin_call(
            "string_join",
            "expected string_join(parts, separator)",
        ));
    }
    let Some(parts) = args[0].with_list(|values| {
        values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                value.with_str(str::to_owned).ok_or_else(|| {
                    invalid_builtin_call(
                        "string_join",
                        format!("part {} is not a string", index + 1),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()
    }) else {
        return Err(invalid_builtin_call(
            "string_join",
            "expected string list as first argument",
        ));
    };
    let separator = builtin_string_arg("string_join", args, 1)?;
    Ok(Value::string(parts?.join(&separator)))
}

fn url_encode_component_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call(
            "url_encode_component",
            "expected url_encode_component(text)",
        ));
    }
    let text = builtin_string_arg("url_encode_component", args, 0)?;
    Ok(Value::string(url_encode_component(&text)))
}

fn url_decode_component_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call(
            "url_decode_component",
            "expected url_decode_component(text)",
        ));
    }
    let text = builtin_string_arg("url_decode_component", args, 0)?;
    let decoded = url_decode_component(&text)
        .map_err(|error| invalid_builtin_call("url_decode_component", error))?;
    Ok(Value::string(decoded))
}

fn url_encode_component(input: &str) -> String {
    let mut out = String::new();
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(hex_digit(byte >> 4));
            out.push(hex_digit(byte & 0x0f));
        }
    }
    out
}

fn url_decode_component(input: &str) -> Result<String, String> {
    let mut bytes = Vec::with_capacity(input.len());
    let mut iter = input.bytes();
    while let Some(byte) = iter.next() {
        match byte {
            b'%' => {
                let hi = iter
                    .next()
                    .ok_or_else(|| "incomplete percent escape".to_owned())?;
                let lo = iter
                    .next()
                    .ok_or_else(|| "incomplete percent escape".to_owned())?;
                let hi = hex_value(hi).ok_or_else(|| "invalid percent escape".to_owned())?;
                let lo = hex_value(lo).ok_or_else(|| "invalid percent escape".to_owned())?;
                bytes.push((hi << 4) | lo);
            }
            b'+' => bytes.push(b' '),
            _ => bytes.push(byte),
        }
    }
    String::from_utf8(bytes).map_err(|_| "decoded component is not valid UTF-8".to_owned())
}

fn sort_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call("sort", "expected sort(list)"));
    }
    let Some(mut values) = args[0].with_list(<[Value]>::to_vec) else {
        return Err(invalid_builtin_call("sort", "expected list argument"));
    };
    values.sort();
    Ok(Value::list(values))
}

fn words_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call("words", "expected words(text)"));
    }
    Ok(Value::list(
        parse_words(&builtin_string_arg("words", args, 0)?)
            .into_iter()
            .map(Value::string),
    ))
}

fn string_starts_with_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 2 {
        return Err(invalid_builtin_call(
            "string_starts_with",
            "expected string_starts_with(text, prefix)",
        ));
    }
    let text = builtin_string_arg("string_starts_with", args, 0)?;
    let prefix = builtin_string_arg("string_starts_with", args, 1)?;
    Ok(Value::bool(text.starts_with(&prefix)))
}

fn string_contains_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 2 {
        return Err(invalid_builtin_call(
            "string_contains",
            "expected string_contains(text, subject)",
        ));
    }
    let text = builtin_string_arg("string_contains", args, 0)?;
    let subject = builtin_string_arg("string_contains", args, 1)?;
    Ok(Value::bool(text.contains(&subject)))
}

fn string_equal_fold_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 2 {
        return Err(invalid_builtin_call(
            "string_equal_fold",
            "expected string_equal_fold(left, right)",
        ));
    }
    let left = builtin_string_arg("string_equal_fold", args, 0)?;
    let right = builtin_string_arg("string_equal_fold", args, 1)?;
    Ok(Value::bool(left.to_lowercase() == right.to_lowercase()))
}

fn edit_distance_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 2 {
        return Err(invalid_builtin_call(
            "edit_distance",
            "expected edit_distance(left, right)",
        ));
    }
    let left = builtin_string_arg("edit_distance", args, 0)?;
    let right = builtin_string_arg("edit_distance", args, 1)?;
    Value::int(levenshtein_chars(&left, &right) as i64)
        .map_err(|_| invalid_builtin_call("edit_distance", "distance is out of range"))
}

fn parse_ordinal_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call(
            "parse_ordinal",
            "expected parse_ordinal(text)",
        ));
    }
    let text = builtin_string_arg("parse_ordinal", args, 0)?;
    match parse_ordinal_text(&text) {
        Some(value) => Value::int(value)
            .map(Value::result_ok)
            .map_err(|_| invalid_builtin_call("parse_ordinal", "ordinal is out of range")),
        None => Ok(Value::result_err(Value::error(
            Symbol::intern("E_PARSE"),
            Some("invalid ordinal"),
            Some(Value::string(text)),
        ))),
    }
}

fn lower_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call("lower", "expected lower(text)"));
    }
    Ok(Value::string(
        builtin_string_arg("lower", args, 0)?.to_lowercase(),
    ))
}

fn os_getenv_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call(
            "os_getenv",
            "expected os_getenv(name)",
        ));
    }
    let name = builtin_string_arg("os_getenv", args, 0)?;
    match std::env::var(&name) {
        Ok(value) => Ok(Value::option_some(Value::string(value))),
        Err(_) => Ok(Value::option_none()),
    }
}

fn parse_words(value: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut escaped = false;

    for ch in value.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '"' {
            in_quotes = !in_quotes;
            continue;
        }
        if ch.is_whitespace() && !in_quotes {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }
        current.push(ch);
    }
    if escaped {
        current.push('\\');
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn levenshtein_chars(left: &str, right: &str) -> usize {
    let left = left.chars().collect::<Vec<_>>();
    let right = right.chars().collect::<Vec<_>>();
    if left.is_empty() {
        return right.len();
    }
    if right.is_empty() {
        return left.len();
    }

    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    let mut current = vec![0; right.len() + 1];
    for (left_index, left_ch) in left.iter().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_ch) in right.iter().enumerate() {
            let substitution = usize::from(left_ch != right_ch);
            current[right_index + 1] = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(previous[right_index] + substitution);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

fn parse_ordinal_text(value: &str) -> Option<i64> {
    let value = value.trim().to_lowercase();
    if value.is_empty() {
        return None;
    }
    if let Some(number) = parse_numeric_ordinal(&value) {
        return Some(number);
    }
    let mut total = 0;
    for part in value.split('-') {
        total += simple_ordinal_value(part)?;
    }
    (total > 0).then_some(total)
}

fn parse_numeric_ordinal(value: &str) -> Option<i64> {
    let trimmed = value
        .strip_suffix("st")
        .or_else(|| value.strip_suffix("nd"))
        .or_else(|| value.strip_suffix("rd"))
        .or_else(|| value.strip_suffix("th"))
        .or_else(|| value.strip_suffix('.'))
        .unwrap_or(value);
    trimmed.parse::<i64>().ok().filter(|value| *value > 0)
}

fn simple_ordinal_value(value: &str) -> Option<i64> {
    match value {
        "first" => Some(1),
        "second" => Some(2),
        "third" => Some(3),
        "fourth" => Some(4),
        "fifth" => Some(5),
        "sixth" => Some(6),
        "seventh" => Some(7),
        "eighth" => Some(8),
        "ninth" => Some(9),
        "tenth" => Some(10),
        "eleventh" => Some(11),
        "twelfth" => Some(12),
        "thirteenth" => Some(13),
        "fourteenth" => Some(14),
        "fifteenth" => Some(15),
        "sixteenth" => Some(16),
        "seventeenth" => Some(17),
        "eighteenth" => Some(18),
        "nineteenth" => Some(19),
        "twenty" | "twentieth" => Some(20),
        "thirty" | "thirtieth" => Some(30),
        "forty" | "fortieth" => Some(40),
        "fifty" | "fiftieth" => Some(50),
        "sixty" | "sixtieth" => Some(60),
        "seventy" | "seventieth" => Some(70),
        "eighty" | "eightieth" => Some(80),
        "ninety" | "ninetieth" => Some(90),
        _ => None,
    }
}

fn string_slice_chars(value: &str, start: usize, end: usize) -> &str {
    let start_byte = value
        .char_indices()
        .nth(start)
        .map(|(index, _)| index)
        .unwrap_or(value.len());
    let end_byte = value
        .char_indices()
        .nth(end)
        .map(|(index, _)| index)
        .unwrap_or(value.len());
    &value[start_byte..end_byte]
}

fn hex_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        10..=15 => (b'A' + (nibble - 10)) as char,
        _ => unreachable!("hex nibble is always below 16"),
    }
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
