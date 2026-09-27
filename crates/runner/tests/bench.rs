use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new(source: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "mica-bench-{}-{}.mica",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, source).unwrap();
        Self(path)
    }

    fn run(&self, expected: &str, extra: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_mica"))
            .arg("bench")
            .arg(&self.0)
            .args([
                "--expected",
                expected,
                "--samples",
                "2",
                "--iterations",
                "2",
                "--warmup",
                "1",
            ])
            .args(extra)
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_file(&self.0).unwrap();
    }
}

#[test]
fn benchmark_waits_for_spawned_work_and_commit_boundaries() {
    let fixture = Fixture::new(
        "verb worker(sender)\n commit()\n mailbox_send(sender, 42)\nend\n\
         verb bench()\n let box = mailbox()\n spawn :worker(sender: box[1])\n\
         let ready = mailbox_recv([box[0]])\n mailbox_close(box[0])\n return ready[0][1][0]\nend",
    );
    for tier in ["interpreter", "native"] {
        let output = fixture.run("42", &["--tier", tier]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["timed_invocations"], 4);
        assert_eq!(report["sample_elapsed_ns"].as_array().unwrap().len(), 2);
        assert_eq!(report["authority"], "root");
        assert_eq!(report["instruction_budget"], 100_000_000);
        assert_eq!(report["max_call_depth"], 1024);
    }
}

#[test]
fn benchmark_rejects_a_changed_result_after_warmup() {
    let fixture = Fixture::new(
        "make_relation(:Counter, 1)\n assert Counter(0)\n\
         verb bench()\n let rows = Counter(?n)\n let n = rows[0][:n]\n\
         retract Counter(n)\n assert Counter(n + 1)\n return n\nend",
    );
    let output = fixture.run("0", &[]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("result mismatch"));
    assert!(output.stdout.is_empty());
}

#[test]
fn benchmark_rejects_failed_setup_and_invalid_counts() {
    let fixture = Fixture::new("verb bench()\n return 1\nend");
    assert!(!fixture.run("1", &["--setup"]).status.success());
    assert!(!fixture.run("1", &["--iterations", "0"]).status.success());
    assert!(!fixture.run("2", &[]).status.success());
}

#[test]
fn benchmark_calibrates_multiple_files_and_reports_validation() {
    let first = Fixture::new("verb bench()\n return 42\nend");
    let second = Fixture::new("verb bench()\n return 42\nend");
    let output = Command::new(env!("CARGO_BIN_EXE_mica"))
        .arg("bench")
        .args([&first.0, &second.0])
        .args([
            "--expected",
            "42",
            "--samples",
            "1",
            "--budget-ms",
            "1",
            "--warmup",
            "0",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reports: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(reports.len(), 2);
    for report in reports {
        assert_eq!(report["result_validated"], true);
        assert_eq!(report["calibration_invocations"], 1);
        assert_eq!(report["budget_ms"], 1);
        assert!(report["iterations_per_sample"].as_u64().unwrap() >= 1);
        assert_eq!(report["timed_invocations"], report["iterations_per_sample"]);
    }
    let unvalidated = Command::new(env!("CARGO_BIN_EXE_mica"))
        .arg("bench")
        .arg(&first.0)
        .args(["--samples", "1", "--iterations", "1", "--warmup", "0"])
        .output()
        .unwrap();
    assert!(unvalidated.status.success());
    let report: Value = serde_json::from_slice(&unvalidated.stdout).unwrap();
    assert_eq!(report["result_validated"], false);
    assert_eq!(report["calibration_invocations"], 0);
    assert!(report["expected"].is_null());
}

#[test]
fn benchmark_rejects_failed_calibration_and_conflicting_sampling_options() {
    let fixture = Fixture::new("verb bench()\n return 42\nend");
    for args in [
        vec!["--budget-ms", "1", "--expected", "41", "--warmup", "0"],
        vec!["--budget-ms", "1", "--iterations", "1"],
        vec!["--budget-ms", "0"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_mica"))
            .arg("bench")
            .arg(&fixture.0)
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
}
