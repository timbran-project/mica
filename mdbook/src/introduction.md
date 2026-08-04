# The Mica Guide

Mica is a programming language and runtime for systems whose information and behaviour continue to
evolve while they are running. It combines persistent data, derived knowledge, installed behaviour,
transactions, and authority in one live environment.

Mica is a programming language and runtime for shared programmable memory. A Mica world is a live
environment where durable identities, facts, rules, verbs, authority, effects, and tasks can all
change while the system is running. Code does not sit outside the data as a separate application
layer. Behaviour is installed into the world alongside the facts it reads and writes.

You do not need experience with Datalog, logic programming, persistent object systems, or MUDs to
use this guide. The tutorial begins with ordinary statements about people, equipment, and work. It
introduces the relational vocabulary only after showing what each idea is for.

The guide is organized in four parts:

- a tutorial that builds the mental model from identities, facts, and questions;
- complete examples that can be loaded and exercised with the Mica runner;
- a systematic language and runtime reference;
- an operations guide for persistent stores, bootstrapping, authority, hosts, and maintenance.

If you are new to Mica, begin with [Start Here](./tutorial/index.md). If you already understand the
model and need exact syntax or semantics, go directly to the
[Language Overview](./language/index.md) or [Runtime Overview](./runtime/index.md).
