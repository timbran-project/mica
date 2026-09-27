// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use mica_runtime::{SourceRunner, TaskOutcome};
use mica_var::Value;

const SOURCES: &[&str] = &[
    include_str!("../../../apps/bycycle-owl/00_schema.mica"),
    include_str!("../../../apps/shared/retrieval.mica"),
    include_str!("../../../apps/bycycle-owl/10_taxonomy.mica"),
    include_str!("../../../apps/bycycle-owl/20_constraints.mica"),
    include_str!("../../../apps/bycycle-owl/30_graph.mica"),
    include_str!("../../../apps/bycycle-owl/40_loader.mica"),
];

#[test]
fn owl_ontology_derives_and_retracts_taxonomy_constraints_and_retrieval() {
    for interpreter_only in [true, false] {
        let mut runner = SourceRunner::new_empty().with_interpreter_only(interpreter_only);
        for source in SOURCES {
            for report in runner.run_filein(source).unwrap() {
                assert!(
                    matches!(report.outcome, TaskOutcome::Complete { .. }),
                    "{}",
                    report.render()
                );
            }
        }
        for report in runner
            .run_filein(
                r#"
            make_identity(:bycycle/robo)
            make_identity(:bycycle/dog)
            make_identity(:bycycle/animal)
            make_identity(:bycycle/living)
            make_identity(:bycycle/artifact)
            make_identity(:bycycle/former)
            make_identity(:bycycle/reader)
            assert Isa(#bycycle/robo, #bycycle/dog)
            assert Isa(#bycycle/robo, #bycycle/artifact)
            assert Genls(#bycycle/dog, #bycycle/animal)
            assert Genls(#bycycle/animal, #bycycle/living)
            assert Genls(#bycycle/dog, #bycycle/living)
            assert DisjointWith(#bycycle/artifact, #bycycle/animal)
            assert QuotedIsa(#bycycle/robo, #bycycle/dog)
            assert TypeGenls(#bycycle/dog, #bycycle/animal)
            assert BroaderTerm(#bycycle/dog, #bycycle/animal)
            assert RewriteOf(#bycycle/former, #bycycle/dog)
            assert RewriteOf(#bycycle/dog, #bycycle/animal)
            assert Label(#bycycle/robo, "RoboDog é🦀")
            assert CanRetrieveSubject(#bycycle/reader, #bycycle/robo)
        "#,
            )
            .unwrap()
        {
            assert!(
                matches!(report.outcome, TaskOutcome::Complete { .. }),
                "{}",
                report.render()
            );
        }
        let report = runner
            .run_source(
                r#"
            require Subsumes(#bycycle/living, #bycycle/dog)
            require InstanceOf(#bycycle/robo, #bycycle/living)
            require IndirectChild(#bycycle/living, #bycycle/dog)
            require !DirectChild(#bycycle/living, #bycycle/dog)
            require DirectChild(#bycycle/animal, #bycycle/dog)
            require DisjointWith(#bycycle/animal, #bycycle/artifact)
            require InconsistentWith(#bycycle/robo, #bycycle/animal, #bycycle/artifact)
            require QuotedInstanceOf(#bycycle/robo, #bycycle/living)
            require TypedInstanceOf(#bycycle/robo, #bycycle/animal)
            require Broader(#bycycle/animal, #bycycle/dog)
            require RewrittenTo(#bycycle/former, #bycycle/animal)
            require TextUnit(#bycycle/robo)
            require TextUnitText(#bycycle/robo, "RoboDog é🦀")
            require CanRetrieveSubject(#bycycle/reader, #bycycle/robo)
            return true
        "#,
            )
            .unwrap();
        assert!(
            matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
            "{}",
            report.render()
        );

        let report = runner
            .run_source(
                r#"
            retract Genls(#bycycle/dog, #bycycle/animal)
            retract DisjointWith(#bycycle/artifact, #bycycle/animal)
            retract Label(#bycycle/robo, _)
            return true
        "#,
            )
            .unwrap();
        assert!(
            matches!(report.outcome, TaskOutcome::Complete { .. }),
            "{}",
            report.render()
        );
        let report = runner
            .run_source(
                r#"
            require !InstanceOf(#bycycle/robo, #bycycle/animal)
            require InstanceOf(#bycycle/robo, #bycycle/living)
            require DirectChild(#bycycle/living, #bycycle/dog)
            require !DisjointWith(#bycycle/animal, #bycycle/artifact)
            require !InconsistentWith(#bycycle/robo, _, _)
            require !TextUnit(#bycycle/robo)
            require !TextUnitText(#bycycle/robo, _)
            return true
        "#,
            )
            .unwrap();
        assert!(
            matches!(report.outcome, TaskOutcome::Complete { ref value, .. } if *value == Value::bool(true)),
            "{}",
            report.render()
        );
    }
}

#[test]
fn owl_batches_commit_facts_and_progress_atomically() {
    for interpreter_only in [true, false] {
        let mut runner = SourceRunner::new_empty().with_interpreter_only(interpreter_only);
        for source in SOURCES {
            runner.run_filein(source).unwrap();
        }
        let report = runner
            .run_source(
                r#"
            return bycycle_load_batch([
                ["animal", [[:Label, "Animal é🦀", false], [:Label, "Beast", false]]],
                ["dog", [[:Genls, "animal", true]]]
            ], [], ["digest", "reader", 2, false], "reader")
        "#,
            )
            .unwrap();
        assert!(
            matches!(report.outcome, TaskOutcome::Complete { ref value, .. }
            if *value == Value::list([Value::int(3).unwrap(), Value::int(0).unwrap()]))
        );
        let report = runner
            .run_source(
                r#"
            require bycycle_progress() == ["digest", "reader", 2, false]
            require Alias(#bycycle/guid/animal, "Beast")
            require InstanceOf(#bycycle/guid/dog, _) == false
            require Subsumes(#bycycle/guid/animal, #bycycle/guid/dog)
            require CanRetrieveSubject(#reader, #bycycle/guid/dog)
            return true
        "#,
            )
            .unwrap();
        assert!(
            matches!(report.outcome, TaskOutcome::Complete { .. }),
            "{}",
            report.render()
        );
        let report = runner
            .run_source(
                r#"
            return bycycle_load_batch([
                ["uncommitted", [[:Label, "Discard", false], [:Unknown, "invalid", false]]]
            ], ["digest", "reader", 2, false], ["digest", "reader", 3, false], "reader")
        "#,
            )
            .unwrap();
        assert!(matches!(report.outcome, TaskOutcome::Aborted { .. }));
        let report = runner
            .run_source(
                r#"
            require !GuidOf(_, "uncommitted")
            require !NamedIdentity(:bycycle/guid/uncommitted, _)
            require !Label(_, "Discard")
            require bycycle_progress() == ["digest", "reader", 2, false]
            return true
        "#,
            )
            .unwrap();
        assert!(
            matches!(report.outcome, TaskOutcome::Complete { .. }),
            "{}",
            report.render()
        );
        let report = runner
            .run_source(
                r#"
            return bycycle_load_batch([], [], ["digest", "reader", 0, true], "reader")
        "#,
            )
            .unwrap();
        assert!(matches!(report.outcome, TaskOutcome::Aborted { .. }));
    }
}
