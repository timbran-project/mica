// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::{
    BuiltinContext, BuiltinRegistry, BuiltinResultKind, RuntimeError, invalid_builtin_call,
    raised_builtin_error,
};
use mica_var::{Value, ValueKind};

pub(super) fn install(registry: BuiltinRegistry) -> BuiltinRegistry {
    registry
        .with_builtin("to_int", BuiltinResultKind::Exact(ValueKind::Int), to_int)
        .with_builtin(
            "to_float",
            BuiltinResultKind::Exact(ValueKind::Float),
            to_float,
        )
        .with_builtin(
            "parse_int",
            BuiltinResultKind::Exact(ValueKind::Int),
            parse_int,
        )
        .with_builtin(
            "parse_float",
            BuiltinResultKind::Exact(ValueKind::Float),
            parse_float,
        )
}

fn argument<'a>(name: &str, args: &'a [Value]) -> Result<&'a Value, RuntimeError> {
    let [value] = args else {
        return Err(invalid_builtin_call(name, "expected one argument"));
    };
    Ok(value)
}

fn to_int(_: &mut BuiltinContext<'_, '_>, args: &[Value]) -> Result<Value, RuntimeError> {
    let value = argument("to_int", args)?;
    if value.as_int().is_some() {
        return Ok(value.clone());
    }
    if let Some(number) = value.as_float()
        && number.fract() == 0.0
        && let Ok(integer) = Value::int(number as i64)
    {
        return Ok(integer);
    }
    Err(raised_builtin_error(
        "E_TYPE",
        "to_int expects an exactly integral number within the integer range",
        None,
    ))
}

fn to_float(_: &mut BuiltinContext<'_, '_>, args: &[Value]) -> Result<Value, RuntimeError> {
    let value = argument("to_float", args)?;
    if value.as_float().is_some() {
        return Ok(value.clone());
    }
    if let Some(number) = value.as_int() {
        return Value::float(number as f32)
            .map_err(|_| raised_builtin_error("E_TYPE", "to_float value is out of range", None));
    }
    Err(raised_builtin_error(
        "E_TYPE",
        "to_float expects a numeric value",
        None,
    ))
}

fn parse_int(_: &mut BuiltinContext<'_, '_>, args: &[Value]) -> Result<Value, RuntimeError> {
    argument("parse_int", args)?
        .with_str(|text| {
            let digits = text.strip_prefix('-').unwrap_or(text);
            if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(raised_builtin_error(
                    "E_INVARG",
                    "parse_int expects decimal digits with an optional leading minus",
                    None,
                ));
            }
            text.parse::<i64>()
                .ok()
                .and_then(|number| Value::int(number).ok())
                .ok_or_else(|| {
                    raised_builtin_error("E_INVARG", "parse_int value is out of range", None)
                })
        })
        .ok_or_else(|| raised_builtin_error("E_TYPE", "parse_int expects a string", None))?
}

fn parse_float(_: &mut BuiltinContext<'_, '_>, args: &[Value]) -> Result<Value, RuntimeError> {
    argument("parse_float", args)?
        .with_str(|text| {
            if !text.bytes().all(|byte| {
                byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.' | b'e' | b'E')
            }) {
                return Err(raised_builtin_error(
                    "E_INVARG",
                    "parse_float expects a decimal float spelling",
                    None,
                ));
            }
            text.parse::<f32>()
                .ok()
                .and_then(|number| Value::float(number).ok())
                .ok_or_else(|| {
                    raised_builtin_error(
                        "E_INVARG",
                        "parse_float spelling is invalid or out of range",
                        None,
                    )
                })
        })
        .ok_or_else(|| raised_builtin_error("E_TYPE", "parse_float expects a string", None))?
}

#[cfg(test)]
mod tests {
    use crate::{SourceRunner, TaskOutcome};
    use mica_var::Value;

    fn result(runner: &mut SourceRunner, source: &str) -> Value {
        let report = runner.run_source(source).unwrap();
        let TaskOutcome::Complete { value, .. } = &report.outcome else {
            panic!("{source}: {}", report.render());
        };
        value.clone()
    }

    #[test]
    fn numeric_conversions_check_integer_boundaries_and_binary32_rounding() {
        for interpreter_only in [true, false] {
            let mut runner = SourceRunner::new_empty().with_interpreter_only(interpreter_only);
            assert_eq!(
                result(
                    &mut runner,
                    r#"
                require to_int(42) == 42
                require to_int(-42.0) == -42
                require to_int(-36028797018963968.0) == -36028797018963968
                require to_float(16777217) == 16777216.0
                require to_float(1.5) == 1.5
                require parse_int("36028797018963967") == 36028797018963967
                require parse_int("-36028797018963968") == -36028797018963968
                require parse_int("-00042") == -42
                require parse_float("1.25e2") == 125.0
                require parse_float("+1.5") == 1.5
                require parse_float("1e-50") == 0.0
                require parse_float("1.4e-45") == 1.4e-45
                return [to_int(2.0), to_float(2), parse_int("2"), parse_float("2")]
            "#
                ),
                Value::list([
                    Value::int(2).unwrap(),
                    Value::float(2.0).unwrap(),
                    Value::int(2).unwrap(),
                    Value::float(2.0).unwrap()
                ])
            );
            for (call, code) in [
                ("to_int(1.5)", "E_TYPE"),
                ("to_int(36028797018963968.0)", "E_TYPE"),
                ("to_int(-3.4028235e38)", "E_TYPE"),
                ("to_int(true)", "E_TYPE"),
                ("to_float(\"2\")", "E_TYPE"),
                ("parse_int(2)", "E_TYPE"),
                ("parse_float(2.0)", "E_TYPE"),
            ] {
                assert_eq!(
                    result(
                        &mut runner,
                        &format!("try\n{call}\ncatch {code}\nreturn true\nend\nreturn false")
                    ),
                    Value::bool(true),
                    "{call}"
                );
            }
        }
    }

    #[test]
    fn numeric_parsers_reject_partial_nondecimal_and_nonfinite_spellings() {
        let mut runner = SourceRunner::new_empty();
        for (function, spellings) in [
            (
                "parse_int",
                &[
                    "",
                    "-",
                    "+1",
                    " 1",
                    "1 ",
                    "1.0",
                    "1_0",
                    "0x10",
                    "١",
                    "36028797018963968",
                    "-36028797018963969",
                    "9999999999999999999999999",
                ][..],
            ),
            (
                "parse_float",
                &[
                    "",
                    ".",
                    "1e",
                    "1e+",
                    " 1.0",
                    "1.0 ",
                    "0x1p2",
                    "1_0",
                    "NaN",
                    "inf",
                    "-Infinity",
                    "3.4028236e38",
                    "1e99",
                    "1.0tail",
                ][..],
            ),
        ] {
            for spelling in spellings {
                let quoted = serde_json::to_string(spelling).unwrap();
                let source = format!(
                    "try\n{function}({quoted})\ncatch E_INVARG\nreturn true\nend\nreturn false"
                );
                assert_eq!(
                    result(&mut runner, &source),
                    Value::bool(true),
                    "{function}({spelling:?})"
                );
            }
        }
    }
}
