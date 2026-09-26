#!/usr/bin/env python3
"""Compare two pinned buffer persistence probes with fresh stores per process."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--before", type=Path, required=True)
    parser.add_argument("--after", type=Path, required=True)
    parser.add_argument("--before-revision", required=True)
    parser.add_argument("--after-revision", required=True)
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--edits", type=int, default=4096)
    parser.add_argument("--scalars", type=int, default=131072)
    parser.add_argument("--cpus", default="5")
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    variants = [
        ("before", args.before.resolve(), args.before_revision),
        ("after", args.after.resolve(), args.after_revision),
    ]
    manifest = {
        "protocol": "buffer-persistence-v1",
        "workers": 1,
        "cpus": args.cpus,
        "accelerator": "none",
        "execution": "release Rust kernel API",
        "rustc": subprocess.check_output(["rustc", "-Vv"], text=True),
        "system": list(os.uname()),
        "binaries": {
            label: {"revision": revision, "sha256": hashlib.sha256(binary.read_bytes()).hexdigest()}
            for label, binary, revision in variants
        },
        "results": [],
    }
    for run in range(args.runs):
        for durability in ["strict", "relaxed"]:
            for label, binary, revision in variants[run % 2:] + variants[:run % 2]:
                name = f"{label}-{durability}-{run}"
                memory = args.output / f"{name}.rss"
                command = [
                    "/usr/bin/time", "-f", "%M", "-o", str(memory),
                    "taskset", "-c", args.cpus, str(binary),
                    str(args.output / f"{name}.store"), str(args.edits),
                    str(args.scalars), durability,
                ]
                process = subprocess.Popen(command, text=True, stdout=subprocess.PIPE,
                                           stderr=subprocess.PIPE, start_new_session=True)
                timed_out = False
                try:
                    stdout, stderr = process.communicate(timeout=120)
                except subprocess.TimeoutExpired:
                    timed_out = True
                    os.killpg(process.pid, signal.SIGKILL)
                    stdout, stderr = process.communicate()
                result = {"variant": label, "revision": revision, "run": run,
                          "durability": durability, "exit_code": process.returncode,
                          "timed_out": timed_out, "stderr": stderr, "stdout": stdout}
                if process.returncode == 0:
                    result["measurement"] = json.loads(stdout)
                    result["peak_rss_kib"] = int(memory.read_text().strip())
                manifest["results"].append(result)
                (args.output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
                print(f"{name}: exit {process.returncode}", flush=True)


if __name__ == "__main__":
    main()
