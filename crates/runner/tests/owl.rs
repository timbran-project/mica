// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com>
// SPDX-License-Identifier: AGPL-3.0-or-later

use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mica-owl-cli-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        let file = fs::File::create(path.join("fixture.owl.gz")).unwrap();
        let mut gzip = GzEncoder::new(file, Compression::default());
        gzip.write_all(include_bytes!(
            "../../../apps/bycycle-owl/testdata/fixture.owl"
        ))
        .unwrap();
        gzip.finish().unwrap();
        Self(path)
    }

    fn run(&self, extra: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_mica"))
            .arg("--store")
            .arg(self.0.join("store"))
            .args(["--durability", "strict", "owl", "--owl"])
            .arg(self.0.join("fixture.owl.gz"))
            .args(extra)
            .output()
            .unwrap()
    }

    fn report(&self, extra: &[&str]) -> Value {
        let output = self.run(extra);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn owl_cli_census_and_durable_gzip_resume() {
    let fixture = Fixture::new();
    let census = fixture.report(&["--census"]);
    assert_eq!(census["subjects"], 5);
    assert!(!fixture.0.join("store").exists());
    let first = fixture.report(&["--init", "--limit", "2", "--retrieval-actor", "reader"]);
    assert_eq!(first["subjects"], 2);
    assert_eq!(first["complete"], false);
    assert_eq!(first["durability"], "strict");
    let second = fixture.report(&["--retrieval-actor", "reader"]);
    assert_eq!(second["subjects"], 2);
    assert_eq!(second["total_subjects"], 4);
    assert_eq!(second["complete"], true);
    assert_eq!(second["sha256"], first["sha256"]);
    let third = fixture.report(&["--retrieval-actor", "reader"]);
    assert_eq!(third["subjects"], 0);
    assert_eq!(third["commits"], 0);
    let changed_actor = fixture.run(&["--retrieval-actor", "other"]);
    assert!(!changed_actor.status.success());
    assert!(String::from_utf8_lossy(&changed_actor.stderr).contains("retrieval actor"));
    assert!(!fixture.run(&["--commit-batch", "0"]).status.success());
}
