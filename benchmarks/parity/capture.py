#!/usr/bin/env python3
"""Capture fixed-work Rust/Odin comparisons from isolated Git revisions."""

import argparse
import datetime
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import shutil
import signal
import statistics
import subprocess
import tarfile


ROOT = Path(__file__).resolve().parents[2]
HARNESS = Path(__file__).resolve().parent


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(command, cwd=None):
    return subprocess.run(command, cwd=cwd, check=True, capture_output=True, text=True).stdout.strip()


def revision(repo, value):
    return run(["git", "-C", str(repo), "rev-parse", f"{value}^{{commit}}"])


def archive(repo, commit, destination):
    destination.mkdir()
    data = subprocess.check_output(["git", "-C", str(repo), "archive", commit])
    with tarfile.open(fileobj=io.BytesIO(data)) as source:
        source.extractall(destination, filter="data")


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def positive(text):
    value = int(text)
    if value < 1:
        raise argparse.ArgumentTypeError("count must be positive")
    return value


def execute(command, cwd, timeout):
    # A timeout owns the whole benchmark process tree, including the time wrapper.
    with subprocess.Popen(command, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          text=True, start_new_session=True) as process:
        try:
            stdout, stderr = process.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.communicate()
            raise
        return subprocess.CompletedProcess(command, process.returncode, stdout, stderr)


def validate_report(report, fixture, protocol, implementation, tier):
    expected = {
        "format": 1, "implementation": implementation, "expected": fixture["expected"],
        "tier": tier, "workers": protocol["workers"], "relation_parallelism": 1,
        "accelerator": "disabled", "accelerator_placements": 0,
        "storage": "memory", "durability": "none", "authority": "root",
        "warmup_invocations": protocol["warmup"],
        "iterations_per_sample": protocol["iterations"],
        "timed_invocations": protocol["samples"] * protocol["iterations"],
    }
    for key, value in expected.items():
        if report.get(key) != value:
            raise ValueError(f"{key}: expected {value!r}, got {report.get(key)!r}")
    samples = report.get("sample_elapsed_ns", [])
    if len(samples) != protocol["samples"] or any(type(n) is not int or n <= 0 for n in samples):
        raise ValueError("missing or invalid elapsed samples")


def fixture_protocol(protocol, fixture):
    selected = dict(protocol)
    mode = fixture.get("invocation_mode", "repeated")
    if mode == "single":
        selected.update(warmup=0, samples=1, iterations=1)
    elif mode != "repeated":
        raise ValueError(f"unknown invocation mode: {mode}")
    return selected


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--rust-revision", default="HEAD")
    parser.add_argument("--odin-repo", type=Path, default=ROOT.parent / "omica")
    parser.add_argument("--odin-revision", default="bfb368c")
    parser.add_argument("--runs", type=positive, default=3)
    parser.add_argument("--samples", type=positive, default=7)
    parser.add_argument("--iterations", type=positive, default=8)
    parser.add_argument("--warmup", type=int, default=2)
    parser.add_argument("--workers", type=positive, default=1)
    parser.add_argument("--cpus", required=True, help="comma-separated allowed Linux CPU ids")
    parser.add_argument("--case", action="append", help="fixture name; repeat for a subset")
    parser.add_argument("--timeout", type=positive, default=180)
    args = parser.parse_args()
    cpus = sorted(set(map(int, args.cpus.split(","))))
    if not cpus or not set(cpus) <= os.sched_getaffinity(0):
        parser.error("CPU ids must belong to the current affinity mask")
    if args.warmup < 0:
        parser.error("warmup must be nonnegative")
    corpus = json.loads((HARNESS / "corpus.json").read_text())
    fixtures = [f for f in corpus["fixtures"] if not args.case or f["name"] in args.case]
    if args.case and set(args.case) != {f["name"] for f in fixtures}:
        parser.error("unknown fixture name")
    for fixture in fixtures:
        if digest(HARNESS / fixture["file"]) != fixture["sha256"]:
            raise ValueError(f"fixture hash differs from manifest: {fixture['name']}")

    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    shutil.copytree(HARNESS, output / "harness", ignore=shutil.ignore_patterns("__pycache__"))
    rust_revision = revision(ROOT, args.rust_revision)
    odin_revision = revision(args.odin_repo, args.odin_revision)
    rust_source, odin_source = output / "rust-source", output / "odin-source"
    archive(ROOT, rust_revision, rust_source)
    archive(args.odin_repo, odin_revision, odin_source)
    shutil.copytree(HARNESS / "odin", odin_source / "tools/paritybench")

    protocol = {key: getattr(args, key) for key in ("runs", "samples", "iterations", "warmup", "workers")}
    protocol.update(cpus=cpus, verification="every invocation, inside timed interval", timeout_seconds=args.timeout)
    odin = shutil.which("odin")
    if odin is None:
        raise RuntimeError("Odin compiler not found")
    builds = [
        (["cargo", "build", "--locked", "--release", "-p", "mica-runner"], rust_source, output / "rust-build.log"),
        ([odin, "build", "tools/paritybench", "-o:speed", f"-out:{output / 'odin-bench'}"], odin_source, output / "odin-build.log"),
    ]
    manifest = {
        "format": 1, "created_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "rust_revision": rust_revision, "odin_revision": odin_revision,
        "rust_compiler": run(["rustc", "-vV"]), "odin_compiler": run([odin, "version"]),
        "machine": {"uname": platform.uname()._asdict(), "cpuinfo": Path("/proc/cpuinfo").read_text()},
        "protocol": protocol, "corpus": corpus, "selected_cases": [f["name"] for f in fixtures],
        "harness_sha256": {str(p.relative_to(HARNESS)): digest(p) for p in sorted(HARNESS.rglob("*"))
                           if p.is_file() and "__pycache__" not in p.parts},
        "builds": [{"command": command, "cwd": str(cwd)} for command, cwd, _ in builds],
        "memory_scope": "fresh-process peak RSS, including loading, setup, warmup, and fixed timed invocations",
        "results": [],
    }
    save(output / "manifest.json", manifest)
    for command, cwd, log in builds:
        print("building", cwd.name, flush=True)
        with log.open("w") as handle:
            subprocess.run(command, cwd=cwd, stdout=handle, stderr=subprocess.STDOUT, check=True)
    rust = rust_source / "target/release/mica"
    manifest["binaries"] = {"rust": digest(rust), "odin": digest(output / "odin-bench")}
    variants = [("rust-interpreter", "rust", "interpreter"), ("odin", "odin", "interpreter"), ("rust-native", "rust", "native-enabled")]
    for repetition in range(args.runs):
        for fixture in fixtures[repetition % len(fixtures):] + fixtures[:repetition % len(fixtures)]:
            selected_protocol = fixture_protocol(protocol, fixture)
            for label, implementation, tier in variants[repetition % 3:] + variants[:repetition % 3]:
                path = output / "harness" / fixture["file"]
                if implementation == "rust":
                    command = [str(rust), "bench", str(path), "--expected", fixture["expected"],
                               "--samples", str(selected_protocol["samples"]), "--iterations", str(selected_protocol["iterations"]),
                               "--warmup", str(selected_protocol["warmup"]), "--workers", str(args.workers),
                               "--tier", "interpreter" if tier == "interpreter" else "native"]
                    if fixture["setup"]:
                        command.append("--setup")
                else:
                    command = [str(output / "odin-bench"), str(path), fixture["expected"], str(selected_protocol["samples"]),
                               str(selected_protocol["iterations"]), str(selected_protocol["warmup"]), str(args.workers), "yes" if fixture["setup"] else "no"]
                stem = f"{fixture['name']}-{label}-{repetition}"
                memory = output / f"{stem}.rss"
                command = ["/usr/bin/time", "-f", "%M", "-o", str(memory),
                           "taskset", "-c", ",".join(map(str, cpus)), *command]
                print(stem, flush=True)
                result = {"case": fixture["name"], "variant": label, "run": repetition, "command": command,
                          "protocol": selected_protocol}
                try:
                    process = execute(command, output, args.timeout)
                    (output / f"{stem}.stdout").write_text(process.stdout)
                    (output / f"{stem}.stderr").write_text(process.stderr)
                    result["exit_code"] = process.returncode
                    if process.returncode:
                        raise ValueError(process.stderr[-3000:])
                    report = json.loads(process.stdout)
                    validate_report(report, fixture, selected_protocol, implementation, tier)
                    result.update(status="passed", report=report, peak_rss_kib=int(memory.read_text().strip()))
                    result["median_ns"] = statistics.median(report["sample_elapsed_ns"]) / selected_protocol["iterations"]
                except (ValueError, subprocess.TimeoutExpired) as error:
                    result.update(status="failed", error=str(error))
                manifest["results"].append(result)
                save(output / "manifest.json", manifest)
    failures = sum(r["status"] != "passed" for r in manifest["results"])
    summary = []
    for fixture in fixtures:
        for label, _, _ in variants:
            rows = [r for r in manifest["results"] if r["case"] == fixture["name"] and r["variant"] == label]
            entry = {"case": fixture["name"], "variant": label, "status": "failed"}
            if len(rows) == args.runs and all(r["status"] == "passed" for r in rows):
                samples = sorted(n / r["protocol"]["iterations"] for r in rows for n in r["report"]["sample_elapsed_ns"])
                entry.update(status="passed", median_ns=statistics.median(r["median_ns"] for r in rows),
                             process_median_range_ns=[min(r["median_ns"] for r in rows), max(r["median_ns"] for r in rows)],
                             p95_sample_ns=samples[max(0, (len(samples) * 95 + 99) // 100 - 1)],
                             peak_rss_kib=[r["peak_rss_kib"] for r in rows])
            summary.append(entry)
    save(output / "summary.json", summary)
    print(f"{len(manifest['results'])} processes, {failures} failures; {output / 'manifest.json'}")
    return bool(failures)


if __name__ == "__main__":
    raise SystemExit(main())
