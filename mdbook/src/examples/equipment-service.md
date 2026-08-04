# Shared Equipment Service

This example turns the tutorial's running domain into an executable system. It models two staff
members, two instruments, two sites, and one project. Rules derive whether an instrument is ready,
who can collect it, and which project is blocked by calibration work.

The complete source is [`apps/examples/equipment-service.mica`](../../../apps/examples/equipment-service.mica).

## Load the World

Create a temporary store and load the owned filein unit:

```sh
export MICA_EXAMPLE_STORE="$(mktemp -d)"

cargo run --bin mica -- \
  --storage fjall --store "$MICA_EXAMPLE_STORE" \
  filein --unit equipment --replace \
  apps/examples/equipment-service.mica
```

The filein installs prototypes, concrete identities, relation declarations, initial facts, three
rules, two verbs, and enough authority policy for Alice and Bob to run the walkthrough.

## Inspect the Initial Conclusions

Bob works at the north office, where Spectrometer 2 is ready for use:

```sh
cargo run --bin mica -- \
  --storage fjall --store "$MICA_EXAMPLE_STORE" --actor bob \
  eval 'return CanCollect(#bob, #spectrometer_2)'
```

The stable returned value is:

```text
true
```

Sensor 17 is assigned to the air-quality project but requires calibration. Ask which assignments
are blocked:

```sh
cargo run --bin mica -- \
  --storage fjall --store "$MICA_EXAMPLE_STORE" --actor alice \
  eval 'return BlockedProject(?project, ?instrument)'
```

The result has two named columns:

```text
[:instrument, :project] {[#sensor_17, #air_quality_project]}
```

The heading order is canonical rather than source-position order, so consume rows by their column
symbols instead of relying on visual order.

## How the Derived Facts Work

Readiness is the absence of a current calibration requirement for a known instrument:

```mica
ReadyForUse(instrument) :-
  Instrument(instrument),
  not RequiresCalibration(instrument)
```

The positive `Instrument` condition bounds the candidates. `not` means the requirement cannot be
derived in the current snapshot.

Collection eligibility chains through readiness:

```mica
CanCollect(person, instrument) :-
  WorksAt(person, site),
  LocatedAt(instrument, site),
  ReadyForUse(instrument)
```

A person and instrument must share a site, and the instrument must be ready. Callers ask one
`CanCollect` question instead of manually joining and checking all three relations.

The blocked-project rule preserves the causal relationship:

```mica
BlockedProject(project, instrument) :-
  AssignedTo(instrument, project),
  RequiresCalibration(instrument)
```

No `BlockedProject` fact is asserted by hand.

## Record Calibration

Alice delegates to the `#technician` prototype, so the calibration verb applies to her:

```sh
cargo run --bin mica -- \
  --storage fjall --store "$MICA_EXAMPLE_STORE" --actor alice \
  eval 'return :record_calibration(actor: #alice, instrument: #sensor_17)'
```

It returns:

```text
:calibrated
```

The verb retracts the maintenance requirement and records who performed the work in one transaction:

```mica
verb record_calibration(actor @ #technician, instrument @ #instrument)
  retract RequiresCalibration(instrument)
  assert CalibratedBy(instrument, actor)
  return :calibrated
end
```

The derived conclusions change without direct updates:

```sh
cargo run --bin mica -- \
  --storage fjall --store "$MICA_EXAMPLE_STORE" --actor alice \
  eval 'return ReadyForUse(#sensor_17)'
```

This now returns `true`. `BlockedProject(#air_quality_project, #sensor_17)` now returns `false`.

## Transfer the Instrument

Move Sensor 17 to Bob's site:

```sh
cargo run --bin mica -- \
  --storage fjall --store "$MICA_EXAMPLE_STORE" --actor alice \
  eval 'return :transfer(actor: #alice, instrument: #sensor_17, destination: #north_office)'
```

It returns `:transferred`. The verb uses functional-relation assignment:

```mica
instrument.locatedAt = destination
instrument.lastTransferredBy = actor
```

Both replacements commit together. The identity `#sensor_17` remains stable; only facts in its
neighbourhood change.

Bob can now collect the calibrated sensor:

```sh
cargo run --bin mica -- \
  --storage fjall --store "$MICA_EXAMPLE_STORE" --actor bob \
  eval 'return CanCollect(#bob, #sensor_17)'
```

The result is `true`. This conclusion follows from the new location, Bob's work site, and the earlier
calibration transition.

## What the Example Demonstrates

- identities remain stable while facts change;
- functional relations enforce one current location and support replacement sugar;
- ordinary relations retain many-valued assignments and calibration records;
- negation and chained rules maintain conclusions from current causes;
- delegation makes concrete actors and instruments match verb roles;
- verbs group domain writes into checked transactional actions;
- actor-scoped CLI tasks receive explicit read, write, and invoke authority;
- the Fjall store carries the installed world across runner processes.

Continue with the [Approval Workflow](./approval-workflow.md) for comparison guards and multiple
rule branches.
