# Installing Behaviour

Facts say what the world currently accepts as true. Rules say what follows from those facts. A
system also needs controlled actions: transfer an instrument, assign it to a project, close a work
order, or approve a repair.

Mica installs these actions as _verbs_.

## A Verb Is Live World Behaviour

Here is a simple transfer verb:

```mica
verb transfer(actor @ #staff, instrument @ #instrument, destination @ #site)
  retract LocatedAt(instrument, _)
  assert LocatedAt(instrument, destination)
  assert LastTransferredBy(instrument, actor)
  return true
end
```

Installing this source adds behaviour to the world. A later task invokes it with named roles:

```mica
:transfer(
  actor: #alice,
  instrument: #sensor_17,
  destination: #north_office
)
```

The selector is `:transfer`. The invocation supplies values for the `actor`, `instrument`, and
`destination` roles.

## Named Roles Replace the Privileged Receiver

Many object systems begin method lookup from one receiver:

```text
sensor.transfer(destination)
```

But transfer behaviour may depend equally on who is acting, what is moving, and where it is going.
Mica does not force one of those values to own the method. Dispatch considers named roles.

This makes multi-party behaviour direct. A calibration action might select a method based on the
technician, the instrument family, and the procedure. No artificial receiver has to stand in for the
whole interaction.

Receiver-call syntax exists where it reads naturally, but it remains sugar for a named role rather
than a separate object lookup mechanism.

## Role Restrictions Select Applicable Methods

The restrictions after `@` describe which values a verb branch accepts:

```mica
actor @ #staff
instrument @ #instrument
destination @ #site
```

Concrete identities match these prototypes through ordinary delegation facts:

```mica
Delegates(#alice, #staff, 0)
Delegates(#sensor_17, #instrument, 0)
Delegates(#north_office, #site, 0)
```

The final number is delegation order. Delegation is stored relationship data, not a parent pointer
hidden inside an object header.

The dispatcher finds installed methods with the requested selector and matching role restrictions.
If more than one candidate remains equally applicable, dispatch reports ambiguity rather than
silently choosing an accidental winner.

## One Selector Can Have Domain-Specific Branches

Suppose hazardous instruments require extra handling:

```mica
verb transfer(
  actor @ #hazmat_technician,
  instrument @ #hazardous_instrument,
  destination @ #containment_site
)
  // Perform the specialized checked transition.
end
```

This branch can coexist with a general transfer method. Dispatch uses the supplied role values and
their delegation relationships to select the applicable behaviour.

The design is closer to multimethod dispatch than to methods stored inside one receiver.

## Verbs Should Implement Checked Transitions

A useful domain verb does more than bundle writes. It establishes the boundary at which the system
checks preconditions, updates related facts, and records consequences:

```mica
verb assign(actor @ #coordinator, instrument @ #instrument, project @ #project)
  ReadyForUse(instrument) || return :not_ready
  assert AssignedTo(instrument, project)
  assert AssignmentMadeBy(instrument, project, actor)
  return :assigned
end
```

The task transaction makes the writes atomic. Authority determines whether the task may invoke the
verb and write its relations. The verb's own checks enforce domain conditions such as readiness.

These are separate layers:

- dispatch answers which behaviour applies;
- authority answers whether this task may invoke and mutate;
- verb logic answers whether this action is valid for the domain;
- the transaction answers whether all changes can commit together.

Do not collapse all four into a single boolean policy relation.

## Behaviour Is Inspectable and Replaceable

Installed methods have durable identities and catalogue facts describing their selector, roles,
restrictions, source, and compiled program. Fileout and inspection tooling can therefore treat
behaviour as part of the world rather than as an invisible Rust-side callback registry.

Filein unit ownership allows a maintained source unit to replace the definitions it owns. This is
how a continuing world can receive updated behaviour without pretending old and new implementations
are unrelated APIs.

## Functions and Verbs Serve Different Purposes

An ordinary function value is useful for local computation, transformation, and higher-order code. A
verb is useful when behaviour should be installed, selected through domain roles, invoked through
the world, inspected, and governed by invoke authority.

Not every helper needs to become a verb. Prefer a verb for a named domain action and ordinary
functions for implementation-local calculation.

## Continue

Installed actions must not grant themselves permission merely by naming an actor. Continue to
[Authority Is Part of the Model](./authority.md).
