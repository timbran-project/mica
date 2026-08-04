# Running the Examples

The examples in this part are complete Mica fileins checked into `apps/examples/`. They are tested by
the runtime test suite and can be exercised through the same runner used for other Mica source.

They use three familiar operational domains:

- the shared equipment service models assets, maintenance, sites, and projects;
- the approval workflow models requests, thresholds, decisions, and domain eligibility;
- the dependency planner models service dependencies and outage impact with recursive rules.

Together they cover the central language model without requiring one large application.

## Run from the Repository Root

The commands assume your working directory is the Mica repository. Cargo builds the runner on the
first invocation, so the first command may take longer than later ones.

Each walkthrough creates a temporary Fjall directory:

```sh
export MICA_EXAMPLE_STORE="$(mktemp -d)"
```

The shell variable is intentionally specific to these examples. It keeps the commands readable and
prevents the examples from writing a database into the source tree.

Use a new temporary directory for each example. The fileins use straightforward unnamespaced
relation names so their domain model is easy to read; they are not intended to be combined in one
store.

## Why Use a Persistent Store Here?

An in-memory filein is enough to check that a file loads:

```sh
cargo run --bin mica -- filein apps/examples/equipment-service.mica
```

The process exits after the filein, so a later `eval` command would start a different empty in-memory
world. A named Fjall store lets the walkthrough load the world in one command and interact with the
same committed state in later commands.

That also demonstrates an essential Mica property: the identities, facts, rules, verbs, and policy
installed by the filein remain available after the original runner process ends.

## Filein Units

Each walkthrough uses a named filein unit and `--replace`:

```sh
filein --unit equipment --replace apps/examples/equipment-service.mica
```

The unit records ownership of installed source definitions and facts. Replacing a unit is the normal
development path when its source changes. It is safer than treating every reload as an unrelated
append operation.

See [Filein and Fileout](../runtime/filein-fileout.md) for the detailed model.

## Actor-Scoped Commands

After bootstrap, the examples use `--actor`:

```sh
--actor alice eval 'return ReadyForUse(#sensor_17)'
```

Each filein contains a small effective `CanRead`, `CanWrite`, and `CanInvoke` policy sufficient for
its walkthrough. The bootstrap filein itself runs with root authority; subsequent tasks demonstrate
ordinary actor-derived authority.

These policy facts are intentionally compact. [Authority](../language/authority.md) explains how a
larger system derives the same effective relations from roles and policy surfaces.

## Reading Runner Output

The runner reports a task number, its returned value, and retry count:

```text
task 1 complete: :transferred (retries: 0)
```

Task numbers depend on the process and store history. The walkthroughs therefore show the stable
returned value rather than promising an exact task number.

Relation results use a heading followed by a set of rows:

```text
[:dependency] {[#api_service], [#database]}
```

Rows are unordered. Their printed order is not an application contract.

## Automated Coverage

`mica-runtime` loads all three checked-in fileins and verifies their important transitions:

```sh
cargo test -p mica-runtime guide_example_stays_executable
```

The tests cover rule results before and after mutations, verb dispatch, rejected workflow actions,
functional relation updates, and recursive dependency effects.

Continue with the [Shared Equipment Service](./equipment-service.md).
