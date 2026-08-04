# Approval Workflow

This example models two purchase requests. An ordinary approver can approve a small request, while a
request over 1,000 units requires a senior approver. The example separates three concerns that are
often conflated:

- request state is stored durable data;
- approval eligibility is a derived domain conclusion;
- runtime authority controls which relations and verbs a submitted task may use.

The complete source is [`apps/examples/approval-workflow.mica`](../../../apps/examples/approval-workflow.mica).

## Load the Workflow

```sh
export MICA_EXAMPLE_STORE="$(mktemp -d)"

cargo run --bin mica -- \
  --storage fjall --store "$MICA_EXAMPLE_STORE" \
  filein --unit approvals --replace \
  apps/examples/approval-workflow.mica
```

The initial requests are:

| Identity | Amount | State |
| --- | ---: | --- |
| `#office_supplies_request` | 250 | `:pending` |
| `#lab_upgrade_request` | 5000 | `:pending` |

Alice is an approver. Sam is a senior approver and delegates transitively to the ordinary approver
prototype for verb dispatch.

## Derive the Threshold Decision

The rule compares the stored amount with a literal threshold:

```mica
NeedsSeniorApproval(request) :-
  Pending(request),
  RequestAmount(request, amount),
  amount > 1000
```

The comparison is a rule guard. `amount` is first bound by the positive `RequestAmount` relation,
then checked.

Two `CanApprove` rules express the alternatives:

```mica
CanApprove(approver, request) :-
  Approver(approver),
  Pending(request),
  not NeedsSeniorApproval(request)

CanApprove(approver, request) :-
  SeniorApprover(approver),
  Pending(request),
  NeedsSeniorApproval(request)
```

Query Alice's eligibility:

```sh
cargo run --bin mica -- \
  --storage fjall --store "$MICA_EXAMPLE_STORE" --actor alice \
  eval 'return CanApprove(#alice, #office_supplies_request)'

cargo run --bin mica -- \
  --storage fjall --store "$MICA_EXAMPLE_STORE" --actor alice \
  eval 'return CanApprove(#alice, #lab_upgrade_request)'
```

The first command returns `true`; the second returns `false`.

## Reject an Ineligible Transition

Alice has runtime invoke and write authority in this teaching fixture, but the domain still says she
is not eligible for the large request:

```sh
cargo run --bin mica -- \
  --storage fjall --store "$MICA_EXAMPLE_STORE" --actor alice \
  eval 'return :approve(actor: #alice, request: #lab_upgrade_request, note: "approved")'
```

The verb returns:

```text
:not_eligible
```

It returns before drafting any state change. The request remains pending.

This distinction is deliberate. Runtime authority answers whether Alice's task may invoke
`:approve` and write the workflow relations. `CanApprove` answers whether this request is valid for
Alice under current business rules.

## Approve with the Senior Role

Sam satisfies the senior rule:

```sh
cargo run --bin mica -- \
  --storage fjall --store "$MICA_EXAMPLE_STORE" --actor sam \
  eval 'return :approve(actor: #sam, request: #lab_upgrade_request, note: "budget confirmed")'
```

The result is `:approved`. Inspect the committed facts as Alice:

```sh
cargo run --bin mica -- \
  --storage fjall --store "$MICA_EXAMPLE_STORE" --actor alice \
  eval 'return [#lab_upgrade_request.requestState, #lab_upgrade_request.approvedBy, #lab_upgrade_request.decisionNote]'
```

The returned list is:

```text
[:approved, #sam, "budget confirmed"]
```

The verb replaced three functional values in one transaction:

```mica
request.requestState = :approved
request.approvedBy = actor
request.decisionNote = note
```

Because `Pending` is derived from `RequestState(request, :pending)`, the pending fact disappears.
Because `Approved` is derived from `RequestState(request, :approved)`, the approved fact appears.
No code updates either derived relation directly.

## Withdrawal Uses a Different Domain Check

The second verb allows only the requester to withdraw a pending request:

```mica
verb withdraw(actor, request @ #request)
  RequestedBy(request, actor) || return :not_requester
  Pending(request) || return :not_pending
  request.requestState = :withdrawn
  return :withdrawn
end
```

The actor role has no dispatch restriction because ownership is a fact that may change independently
of prototype structure. The verb queries that fact as a precondition instead.

This is a useful design choice: use dispatch restrictions for stable applicability categories and
ordinary relational checks for current domain state.

## What the Example Demonstrates

- comparison guards in rules;
- stratified negation over a bounded positive relation;
- multiple rules providing alternative reasons for one conclusion;
- symbols as compact workflow-state values;
- functional relations for current state and decision metadata;
- domain eligibility kept separate from runtime authority;
- early return before a rejected task drafts writes;
- derived state changing automatically after a committed transition.

Continue with the [Recursive Dependency Planner](./dependency-planner.md).
