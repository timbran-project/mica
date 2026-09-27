// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new(source: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "mica-eval-{}-{}.mica",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, source).unwrap();
        Self(path)
    }

    fn evaluate(&self, actor: Option<&str>, source: &str) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mica"));
        if let Some(actor) = actor {
            command.args(["--actor", actor]);
        }
        command
            .args(["eval", "--filein"])
            .arg(&self.0)
            .arg(source)
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_file(&self.0).unwrap();
    }
}

fn assert_complete(output: Output, expected: &str) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(&format!("complete: {expected} (")),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn eval_filein_invokes_loaded_verbs_with_local_administrative_authority() {
    let fixture = Fixture::new(
        "verb add(left, right)\n return left + right\nend\n\
         verb answer()\n return :add(left: 40, right: 2)\nend",
    );
    assert_complete(fixture.evaluate(None, "return answer()"), "42");
}

#[test]
fn actor_evaluation_keeps_policy_derived_authority() {
    let fixture = Fixture::new(
        "make_identity(:alice)\n\
         make_relation(:CanInvoke, 2)\n\
         assert CanInvoke(#alice, :allowed)\n\
         verb allowed()\n return 42\nend\n\
         verb denied()\n return 99\nend",
    );
    assert_complete(fixture.evaluate(Some("alice"), "return allowed()"), "42");
    let denied = fixture.evaluate(Some("alice"), "return denied()");
    assert!(!denied.status.success());
    assert!(
        String::from_utf8_lossy(&denied.stderr).contains("no applicable method for :denied"),
        "{denied:?}"
    );
    assert_complete(fixture.evaluate(None, "return denied()"), "99");
}
