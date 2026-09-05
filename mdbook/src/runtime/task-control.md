# Task Control

Task-control forms let a computation publish its work and cooperate with the runtime driver. Each
suspending form commits the current transaction before waiting. The driver determines when to
resume the continuation and supplies its result value. See
[Tasks and Transactions](./tasks-and-transactions.md) for commit, replay, and authority rules.

## Publishing and Waiting

`commit()` publishes the current transaction and yields to the driver. Execution continues in a
fresh transaction when scheduled:

```mica
assert Ready(#job)
commit()
```

`suspend(seconds)` waits for a duration measured in seconds. Both integers and floats can express a
duration:

```mica
suspend(1.5)
```

A zero-duration suspension still crosses a transaction boundary. `suspend()` without a duration
keeps the continuation available for an explicit host resumption; it does not arrange a timer.
These forms cooperate with the scheduler. They do not block the worker thread for the duration of
the wait.

`read(metadata)` waits for input addressed to the task's endpoint:

```mica
let line = read(:line)
```

The metadata describes the request to the host. `:line` is a value passed to that host protocol; the
VM itself does not read a terminal or assume every input is a string. The value supplied by the
host becomes the value of the `read` expression. Multiple suspended readers on an endpoint can
receive the same input, so use one reader when a protocol requires a single consumer.

## Starting a Child

`spawn` takes a dispatch expression and returns the child task's integer id:

```mica
let child = spawn :tick(actor: actor(), clock: #clock) after 5
```

The optional `after` duration delays the child's invocation. The parent commits first, so the child
can observe the parent's published facts. The parent resumes once the child is submitted; it does
not wait for the child to finish and does not receive the child's return value.

```mica
assert AssignedTo(#task17, #worker)
let child = spawn :work(agent: #worker, task: #task17)
```

The child's execution context comes from its parent. Naming `agent: #worker` supplies a dispatch
role; it does not switch the child to that identity's authority. Current policy is used when the
child starts and when a delayed child resumes. A child has its own transactions, errors, and
completion. Use explicit facts or messages when its parent needs to observe its progress.

## Mailboxes

`mailbox()` returns two capabilities for one fresh queue:

```mica
let [rx, tx] = mailbox()
```

Keep `rx` with the consumer and give `tx` to producers. The send capability permits delivery; it
does not permit receiving or closing the mailbox. `mailbox_send(tx, value)` buffers a message for
delivery at the sender's next successful commit and returns the sent value.

`mailbox_recv(receivers, timeout?)` waits on a list of receive capabilities:

```mica
let ready = mailbox_recv([rx1, rx2], 1)
```

The optional timeout is measured in seconds. With no timeout, the task waits for a ready mailbox.
With `0`, it polls. A positive timeout waits up to that duration. A poll or timeout with no messages
returns `[]`.

A successful receive returns a list of groups, each shaped as `[receiver, messages]`. It drains all
currently queued messages for each ready receiver. Empty mailboxes contribute no group. Receivers
are considered in the order supplied, and repeating a receiver does not drain it twice.

```mica
let ready = mailbox_recv([rx1, rx2], 1)
for group in ready
  let [receiver, messages] = group
  for message in messages
    handle_message(receiver, message)
  end
end
```

Receiving commits even when a message is already queued. Code after the receive runs against a new
snapshot. The delivered messages become local values in the continuation, so a later transaction
retry does not drain the queues again.

A worker can report its result through a send capability passed as a role:

```mica
let [rx, tx] = mailbox()
spawn :fetch(agent: #worker, reply_to: tx)

try
  let ready = mailbox_recv([rx], 10)
  if ready == []
    raise E_TIMEOUT, "The worker did not reply."
  end
  for group in ready
    for message in group[1]
      record_reply(message)
    end
  end
finally
  mailbox_close(rx)
end
```

The worker must call `mailbox_send` and reach a commit boundary for a reply to arrive. Its ordinary
return value is separate from the mailbox protocol. Include an operation identity in messages when
one queue carries replies for several requests.

## Resource Lifetimes

`mailbox_close(rx)` immediately closes the live queue, revokes both capabilities, and discards queued
messages. Cancel change subscriptions attached to a language-created mailbox before closing it.
An external producer attempting delivery through a closed mailbox observes failure. A send already
buffered by another task is discarded if the mailbox has closed by the time that task commits.

Creation and closure manage ephemeral resources; they are not relation writes that an aborted
transaction restores. Close a mailbox when its consumer is finished. Its capabilities can travel
through local values, call arguments, and other mailboxes, while durable facts store progress such
as `ToolResult`, `Observation`, or `Completed`.

A host may cancel a suspended task or close the endpoint that owns it. Cancellation discards the
continuation; it does not resume the VM to run a `finally` block. Hosts should therefore close the
resources they own as part of cancellation, and application protocols should make abandoned work
recognizable in durable state. A task that reaches its own `finally` block through ordinary return,
break, or error unwinding can perform language-level cleanup there.

Relation subscriptions deliver committed changes through the same mailbox mechanism. See
[Subscriptions](./subscriptions.md) for registration, bounded delivery, and resynchronization.
