# Errors and Recovery

Mica errors are values that can unwind the current task or be handled inside the task. Error-code
literals begin with `E_`:

```mica
E_PERMISSION
E_NOT_FOUND
```

Use `raise` to signal an error:

```mica
raise E_NOT_FOUND
raise E_PERMISSION, "Approval is not permitted."
raise E_PERMISSION, "Approval is not permitted.", request
```

The optional second value is the message. The optional third value is a payload chosen by the
program.

`try` handles errors for a block of code:

```mica
try
  risky()
catch E_PERMISSION as err
  match err.message
  case some(message)
    emit(actor, message)
  case none
    emit(actor, "Permission denied.")
  end
catch
  emit(actor, "Something went wrong.")
finally
  cleanup()
end
```

The compiled form supports catch-all clauses, error-code literal matches, and boolean catch
conditions. `as err` binds the error value in the catch body. `finally` runs cleanup code when
control leaves the protected region.

Errors expose three built-in fields unless a relation-backed dot name shadows them:

```mica
err.code
err.message
err.value
```

`code` is always present. `message` is `option<string>` and `value` is `option<dynamic>` because a
raised error may omit either field.

`recover` is the expression-level form. It evaluates an expression and maps selected errors to
replacement values:

```mica
let description = recover item.description
catch E_CARDINALITY => "It is hard to describe."
catch => "You see nothing special."
end
```

Like `try`, compiled `recover` clauses can match error-code literals, catch all, or test a boolean
condition against a bound error value.

Errors are not limited to a fixed built-in list. The compiler recognizes any identifier beginning
with `E_` as an error-code literal.
