# Mica Guide Examples

These fileins back the complete examples in `mdbook/src/examples/`:

- `equipment-service.mica` introduces identities, functional and ordinary relations, derived facts,
  negation, role dispatch, and transactional changes;
- `approval-workflow.mica` models a business workflow with comparison rules and explicit domain
  eligibility;
- `dependency-planner.mica` uses positive recursion to derive transitive dependencies and outage
  impact.

Load each example into a separate store. The guide uses temporary Fjall stores so state persists
between filein and eval commands without mixing the examples together.
