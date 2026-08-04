# Task Control

Task-control forms are how Mica code cooperates with the runtime driver. They are used when a task
needs to publish a transaction, wait for time to pass, wait for input, start another task, or
coordinate with another task.

`commit()` commits the current transaction and resumes the task immediately:

```mica
commit()
```

`suspend(seconds)` commits and resumes later:

```mica
suspend(1.5)
```

`read(metadata)` commits and waits for endpoint input:

```mica
let line = read(:line)
```

`spawn` creates a child task from a dispatch expression:

```mica
let child = spawn :tick(actor: actor(), clock: #clock) after 5
```

Creating the child is a transaction boundary. The parent commits, the child is submitted against the
committed world, and the parent resumes with the child task id.

For agent-style workflows, this lets a planner publish enough state for a worker task to see a
coherent assignment:

```mica
assert AssignedTo(#task17, #worker)
let child = spawn :work(agent: #worker, task: #task17)
```

`mailbox()` creates a fresh ephemeral mailbox:

```mica
let [rx, tx] = mailbox()
```

`mailbox_send(tx, value)` buffers a message for delivery at the sender's next commit boundary.

`mailbox_recv(receivers, timeout?)` commits and waits on a list of receive caps:

```mica
let ready = mailbox_recv([rx1, rx2], 1)
```

The result is a list of ready groups. Each group is `[rx, messages]`.

The timeout is optional. With no timeout, the task waits until at least one mailbox is ready. With
timeout `0`, the task polls and resumes immediately. With a positive timeout, the task waits up to
that many seconds.

A common pattern is to give a worker a send cap and keep the receive cap in the planner:

```mica
let [rx, tx] = mailbox()
spawn :fetch(agent: #worker, reply_to: tx)

let ready = mailbox_recv([rx], 10)
```

Mailbox values are capabilities, not durable relation facts. They are used to coordinate live tasks,
while durable progress should still be written as facts such as `ToolResult`, `Observation`, or
`Completed`.

Relation subscriptions deliver their settled changes through the same mailbox mechanism. See
[Subscriptions](./subscriptions.md) for `subscribe_changes`, bounded delivery, and
resynchronization.
