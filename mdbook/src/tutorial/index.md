# Start Here

Mica is for software that has to remember, reason, and change over time.

Consider a shared equipment service. People register instruments, move them between sites, assign
them to projects, record inspections, and decide who may approve repairs. The service must keep its
state across restarts. Several people and automated processes may act at once. A change to one fact
may affect many conclusions: moving a sensor can change who can collect it, which work is blocked,
and which dashboard rows are visible.

A conventional implementation might divide this system among application objects, database rows,
authorization middleware, background jobs, and event handlers. Mica offers another shape: put the
identities, facts, rules, behaviours, and policy into one live relational world, then run each action
as a transaction over that world.

This tutorial explains that sentence piece by piece. It assumes you can read small code examples,
but it does not assume database theory, logic programming, or experience with persistent
programming systems.

## What You Will Learn

The tutorial follows one equipment-tracking example and introduces these ideas in order:

1. A Mica program can install definitions and state into a persistent environment.
2. An identity such as `#sensor_17` remains the same thing while its facts change.
3. A relation is a named kind of fact, not an object field or an SQL query.
4. A query describes the facts you want and returns the matching bindings.
5. Every task changes a private draft before publishing it atomically.
6. A rule derives facts from other facts, so conclusions stay consistent with their causes.
7. A verb installs behaviour and dispatches by named roles rather than one special receiver.
8. Durable policy facts are compiled into cheap, ephemeral authority for a task.

You will not need all of Mica's syntax at once. Each chapter introduces only the language needed for
its idea and links to the reference for the complete rules.

## The Example Domain

The examples describe a small organization with people, equipment, sites, and service work:

```text
Alice works at the calibration lab.
Sensor 17 is at the calibration lab.
Sensor 17 requires calibration.
Work order 42 concerns Sensor 17.
```

Mica stores statements like these directly:

```mica
WorksAt(#alice, #calibration_lab)
LocatedAt(#sensor_17, #calibration_lab)
RequiresCalibration(#sensor_17)
Concerns(#work_order_42, #sensor_17)
```

The capitalized names are relations. Values inside the parentheses identify the things that
participate in each fact. There are no tables, classes, inheritance trees, or rule engines to learn
before that notation is useful: read each line as a statement the running system currently accepts
as true.

## What Mica Means by a World

This guide uses *world* for the complete live environment: its identities, stored facts, derived
facts, installed rules and verbs, and policy. A world can model a lab, a company, a document
collection, a simulation, an agent workspace, or an operational service.

The word is useful because the environment is more than a database. Code can be installed into it,
people and processes can act on it concurrently, and its definitions can change without rebuilding
the runtime around a new set of Rust structures.

## A Note About Datalog

Mica's rules are influenced by Datalog, a small relational logic language. You do not need to learn
Datalog separately. In this tutorial, a rule is simply a reusable statement of the form:

> Whenever these facts are true, treat this other fact as true too.

The [Deriving New Knowledge](./rules.md) chapter develops that model with ordinary examples before
naming the more formal properties.

## Continue

Begin with [A Program That Keeps Its State](./persistent-world.md). It explains the persistent
programming model and why loading a Mica file is different from merely starting an application
process.
