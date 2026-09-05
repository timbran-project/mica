# Errors and Recovery

Mica errors are values that can unwind the current task or be handled inside the task. Error-code
literals begin with `E_`:

```mica
E_PERMISSION
E_NOT_FOUND
```

Three related forms serve different purposes:

| Form                                  | Meaning                                                         |
| ------------------------------------- | --------------------------------------------------------------- |
| `E_NOT_FOUND`                         | an error-code value, suitable for comparison or raising         |
| `error(E_NOT_FOUND, "Missing label")` | an error value containing a code, message, and optional payload |
| `err(problem)`                        | an ordinary returned result containing an error value           |

Evaluating an error code or returning `err(problem)` does not unwind anything. `raise` transfers
control to a matching handler, or aborts the current transaction when no handler accepts it. Use
results for expected outcomes that callers should inspect and raised errors for failures that should
interrupt the current computation.

## Constructing Error Values

`error(code[, message[, payload]])` constructs an error without raising it. Omit the message or pass
`none` to leave it absent; an empty string is a present, empty message. Supplying a third argument
records a present payload, including when that argument is `()` or `none`.

```mica,eval
let problem = error(E_NOT_FOUND, "Missing label", :label)
require problem.code == E_NOT_FOUND
require problem.message == some("Missing label")
require problem.value == some(:label)
require error(E_NOT_FOUND, none, ()).value == some(())
require from_literal(to_literal(problem)) == ok(problem)
return err(problem)
```

An integration can create an error code from any symbol with `error_code(symbol)`. For example,
`error_code(:ExternalTimeout)` represents a host code whose name does not use the `E_` source
convention. Error codes remain distinct from the symbols with the same names.

`to_literal` writes structured errors using these constructors. `from_literal` recognizes the
constructors and recursively decodes their literal arguments. It does not evaluate source code, look
up arbitrary functions, or call verbs while decoding. This permits errors nested in lists, maps,
relations, and other persistable values to cross the text boundary with their fields intact.

## Raising and Inspecting an Error

Use `raise` to signal an error:

```mica
raise E_NOT_FOUND
raise E_PERMISSION, "Approval is not permitted."
raise E_PERMISSION, "Approval is not permitted.", request
```

The optional second value is the message. The optional third value is a payload chosen by the
program. The message is text for a person; use the code and structured payload when a caller needs
to make a decision. A caller should not need to parse an English message to recognize a failure.

An existing error can be raised directly with `raise problem`. This preserves its code, message, and
payload. `raise problem.code` constructs a fresh error from just the code, so use the former when
forwarding a caught error or converting an `err` result into a raised failure.

## Handling a Block

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

Catch clauses are tried in source order. A code literal matches that code; a plain `catch` accepts
any raised error. `as err` binds the full error in the catch body. Put specific handlers before a
catch-all so they can run.

A conditional catch binds an error and tests a condition:

```mica,eval
let recovered = recover raise E_RETRY, "Try again.", :temporary
catch problem if problem.value == some(:temporary) => :retry
end
require recovered == :retry
```

When the condition is false, the next clause is considered. When no clause matches, the error
continues outward. An error raised by a catch body goes to an enclosing handler; the sibling clauses
of the same `try` are not another attempt at handling that body's error.

`finally` runs when normal execution, a return, an error, or loop control leaves its protected
region. Nested finalizers run from the innermost region outward. A loop entirely inside the region
can break without leaving it; cleanup then waits for the enclosing `try` to finish.

Keep finalizers focused on cleanup. A `return` or escaping `raise` inside a finalizer replaces the
pending return or error. An error caught inside the finalizer allows the original control flow to
continue afterward. Suspending during cleanup preserves that pending control flow until resumption.
Host cancellation has a different lifetime: it discards the continuation rather than executing
language cleanup. See [Task Control](../runtime/task-control.md#resource-lifetimes).

Errors expose three built-in fields unless a relation-backed dot name shadows them:

```mica
err.code
err.message
err.value
```

`code` is always present. `message` is `option<string>` and `value` is `option<dynamic>` because a
raised error may omit either field.

```mica,eval
try
  raise E_MISSING, "No label was provided.", :label
catch E_MISSING as problem
  require problem.code == E_MISSING
  require problem.message == some("No label was provided.")
  require problem.value == some(:label)
end
```

## Replacing an Expression Result

`recover` is the expression-level form. It evaluates an expression and maps selected errors to
replacement values:

```mica
let description = recover item.description
catch E_CARDINALITY => "It is hard to describe."
catch => "You see nothing special."
end
```

Like `try`, compiled `recover` clauses can match error-code literals, catch all, or test a boolean
condition against a bound error value. The protected expression is evaluated once. If it succeeds,
its value is the result of `recover`; if a handler accepts its error, that handler's expression
provides the result. Unmatched errors continue to the surrounding context.

## Errors and State

Catching an error continues the same transaction. It does not restore the local bindings or facts
changed before the raise:

```mica,eval
make_relation(:Attempted, 1)
try
  assert Attempted(:inspection)
  raise E_RETRY
catch E_RETRY
  require Attempted(:inspection)
end
require Attempted(:inspection)
```

If the error escapes the task, the current transaction's pending writes and output are discarded.
Earlier committed transactions remain published. Choose the location of a handler and of commit
boundaries together: a handler can turn an error into a successful task whose preceding draft
changes will commit.

`require condition` is a task assertion. A falsey condition aborts the task directly with
`"require failed"`; it does not raise a catchable application error or unwind through `finally`. Use
`if ... raise E_...` when the failure belongs in the language's recovery protocol. Runtime failures
such as an exceeded instruction budget or a denied authority check also reach the host through its
runtime error path, separately from raised language errors.

Errors are not limited to a fixed built-in list. The compiler recognizes any identifier beginning
with `E_` as an error-code literal.
