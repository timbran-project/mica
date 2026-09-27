// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use mica_runtime::{SourceRunner, TaskOutcome};

#[test]
fn cycl_sample_keeps_microtheories_separate_from_owl_and_catalogue() {
    for interpreter_only in [true, false] {
        let mut runner = SourceRunner::new_empty().with_interpreter_only(interpreter_only);
        runner
            .run_filein(include_str!("../../../apps/bycycle-owl/00_schema.mica"))
            .unwrap();
        for source in [
            include_str!("../../../apps/bycycle/00_schema.mica"),
            include_str!("../../../apps/bycycle/10_sample.mica"),
        ] {
            for report in runner.run_filein(source).unwrap() {
                assert!(
                    matches!(report.outcome, TaskOutcome::Complete { .. }),
                    "{}",
                    report.render()
                );
            }
        }
        let report = runner
            .run_source(
                r#"
            require len(Cyc/Isa(?item, ?kind, ?mt)) == 5
            require len(Cyc/Genls(?item, ?kind, ?mt)) == 2
            require len(Cyc/Isa(?person, #cyc/dentist, #cyc/people_data_mt)) == 2
            require !Cyc/Isa(_, #cyc/dentist, #cyc/base_kb)
            require !Isa(_, _)
            assert Cyc/Arity(:isa, 2, #cyc/base_kb)
            require Cyc/Arity(:isa, 2, #cyc/base_kb)
            require Arity(_, 3)
            return true
        "#,
            )
            .unwrap();
        assert!(
            matches!(report.outcome, TaskOutcome::Complete { .. }),
            "{}",
            report.render()
        );
    }
}
