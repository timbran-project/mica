// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use mica_relation_kernel::{Atom, Rule, RuleBodyItem, RuleComparisonOp, RuleGuard, Term};

use super::{
    Options, RelationKernel, RelationMetadata, Result, Step, Symbol, compare_trace, id, integer,
    next, reference,
};

// Matches oracle_rules in apps/native/kernel/tests.c. Holes become fresh,
// unreferenced variables in Rust. Negative terms encode the constant -term-1.
const HOLE: i32 = -100;
fn term(n: i32) -> Term {
    match n {
        HOLE => Term::Var(Symbol::intern("hole")),
        n if n < 0 => Term::Value(integer(i64::from(-n - 1))),
        n => Term::Var(Symbol::intern(&format!("v{n}"))),
    }
}
fn atom(relation: u64, negative: bool, terms: [i32; 2]) -> RuleBodyItem {
    let terms = terms.map(term);
    if negative {
        Atom::negated(id(relation), terms).into()
    } else {
        Atom::positive(id(relation), terms).into()
    }
}
pub(super) fn kernel() -> RelationKernel {
    let kernel = RelationKernel::new();
    for relation in 1..16 {
        kernel
            .create_relation(RelationMetadata::new(
                id(relation),
                Symbol::intern(&format!("RuleR{relation}")),
                2,
            ))
            .unwrap();
    }
    let install = |head, terms: [i32; 2], body: Vec<RuleBodyItem>| {
        kernel
            .install_rule(
                Rule::new(id(head), terms.map(term), body),
                "differential fixture",
            )
            .unwrap();
    };
    install(2, [0, 1], vec![atom(1, false, [0, 1])]);
    install(
        2,
        [0, 2],
        vec![atom(2, false, [0, 1]), atom(1, false, [1, 2])],
    );
    install(
        2,
        [0, 2],
        vec![atom(2, false, [0, 1]), atom(2, false, [1, 2])],
    );
    install(3, [0, 1], vec![atom(2, false, [0, 1])]);
    install(2, [0, 1], vec![atom(3, false, [0, 1])]);
    install(
        5,
        [0, 1],
        vec![atom(4, true, [0, 1]), atom(2, false, [0, 1])],
    );
    let guard = |op| RuleGuard::new(op, term(0), term(-2)).into();
    install(
        6,
        [0, 1],
        vec![atom(5, false, [0, 1]), guard(RuleComparisonOp::Lt)],
    );
    install(7, [0, 0], vec![atom(1, false, [0, 0])]);
    install(4, [0, 1], vec![atom(7, false, [0, 1])]);
    install(8, [-3, 0], vec![atom(1, false, [0, -2])]);
    install(9, [0, 0], vec![atom(1, false, [0, HOLE])]);
    install(2, [0, 1], vec![atom(2, false, [0, 1])]);
    install(2, [0, 1], vec![atom(1, false, [0, 1])]);
    for (offset, op) in [
        RuleComparisonOp::Eq,
        RuleComparisonOp::Ne,
        RuleComparisonOp::Lt,
        RuleComparisonOp::Le,
        RuleComparisonOp::Gt,
        RuleComparisonOp::Ge,
    ]
    .into_iter()
    .enumerate()
    {
        install(
            10 + offset as u64,
            [0, 1],
            vec![atom(5, false, [0, 1]), guard(op)],
        );
    }
    kernel
}
fn inspect(steps: &mut Vec<Step>, slot: usize) {
    for relation in 1..16 {
        steps.push(Step::new('q', slot, relation, 0, 0, 0));
    }
}
fn corpus(options: &Options) -> Vec<Step> {
    let mut steps = Vec::new();
    // Start with a cycle and two supports; then retract the asserted head,
    // remove each support, add/remove a blocker, and empty the recursive SCC.
    let transitions = [
        ('a', 1, 0, 1),
        ('a', 1, 1, 2),
        ('a', 1, 0, 2),
        ('a', 1, 2, 0),
        ('a', 2, 0, 2),
        ('r', 2, 0, 2),
        ('r', 1, 0, 2),
        ('a', 4, 0, 2),
        ('r', 4, 0, 2),
        ('r', 1, 0, 1),
        ('r', 1, 1, 2),
        ('r', 1, 2, 0),
    ];
    let mut seed = options.seed;
    for case in 0..options.cases.get() as usize + transitions.len() {
        steps.push(Step::control('b', 1)); // Hold the committed snapshot.
        inspect(&mut steps, 1);
        steps.push(Step::control('b', 0));
        let scripted = case < transitions.len();
        let edits = if scripted { 1 } else { 4 };
        for _ in 0..edits {
            let (op, relation, a, b) = if scripted {
                transitions[case]
            } else {
                (
                    if next(&mut seed).is_multiple_of(2) {
                        'a'
                    } else {
                        'r'
                    },
                    [1, 2, 4][next(&mut seed) as usize % 3],
                    (next(&mut seed) % 6) as i64,
                    (next(&mut seed) % 6) as i64,
                )
            };
            steps.push(Step::new(op, 0, relation, a, b, 0));
            inspect(&mut steps, 0);
        }
        if scripted || !case.is_multiple_of(4) {
            steps.push(Step::control('c', 0));
        }
        steps.push(Step::control('e', 0));
        steps.push(Step::control('g', 0));
        inspect(&mut steps, 1); // Old snapshots retain their complete closure.
        steps.push(Step::control('e', 1));
        steps.push(Step::control('b', 0));
        inspect(&mut steps, 0);
        steps.push(Step::control('e', 0));
    }
    steps
}
pub(super) fn correctness(options: &Options) -> Result<()> {
    let steps = corpus(options);
    compare_trace(
        options,
        "rules",
        &steps,
        &reference(&steps, &kernel(), true),
    )
}
