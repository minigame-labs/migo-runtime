#!/usr/bin/env python3
"""Fresh-process ABBA/BAAB measurements of actual HTTP/OffWorker/InnerAudio.

Uses one retained Release binary and identical PCM hashes for all quantum arms.
The virtual player requests a 1ms poll on underflow and runs faster than real
time. Client/listener/fixture setup is outside the interval; codec construction,
HTTP request, handoffs, resampling, player buffering/mixing and hashing are in
it. This is a host throughput/CPU probe, not hardware playback or power evidence.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import statistics
import subprocess


def sha(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def marked(text: str, prefix: str) -> list[dict]:
    rows = []
    for line in text.splitlines():
        if prefix in line:
            rows.append(json.loads(line.split(prefix, 1)[1]))
    return rows


def main(cohort: Path, output: Path, blocks: int, repeats: int, base_batch: int = 1,
         alternatives: list[int] | None = None) -> None:
    alternatives = alternatives or [4, 8, 16]
    assert base_batch not in alternatives
    assert not output.exists(), "use a fresh measurement directory"
    assert blocks >= 4 and blocks % 2 == 0 and repeats >= 3
    summary = json.loads((cohort / "summary.json").read_text())
    binary = cohort / "positive-tests"
    assert sha(binary) == summary["test_binary_sha256"]
    output.mkdir(parents=True)
    driver = Path(__file__).resolve()
    (output / "measurement-driver.py").write_bytes(driver.read_bytes())
    cases = {}
    for row in marked((cohort / "tests-validated.log").read_text(), "W09_PLAYER_ORACLE "):
        key = (row["fixture"], row["target_rate"])
        assert cases.get(key, row) == row, "different quantum arms disagreed with the oracle"
        cases[key] = row
    selected = [("long.mp3", 44100), ("long.mp3", 48000), ("cbr-44100-1.mp3", 48000)]
    assert all(key in cases for key in selected)
    allowed = sorted(os.sched_getaffinity(0))
    cpu = allowed[0]
    environment = {"allowed_cpus": allowed, "child_cpu": cpu, "kernel": os.uname().release,
                   "binary_sha256": sha(binary), "driver_sha256": sha(driver),
                   "base_batch":base_batch,"alternatives":alternatives,"blocks": blocks, "repeats_per_process": repeats,
                   "scope": __doc__, "clock": "CLOCK_PROCESS_CPUTIME_ID and monotonic Instant",
                   "allocation_scope": "IO executor thread; separately direct decoder/resampler steps, constructor outside the step excluded",
                   "uncertainty": "block ranges are descriptive; no device, power or whole-runtime claim"}
    (output / "environment.json").write_text(json.dumps(environment, indent=2) + "\n")
    (output / "cpuinfo.txt").write_text(Path("/proc/cpuinfo").read_text())
    raw = []
    for name, rate in selected:
        oracle = cases[(name, rate)]
        for alternative in alternatives:
            for block in range(blocks):
                order = [base_batch, alternative, alternative, base_batch] if block % 2 == 0 else [alternative, base_batch, base_batch, alternative]
                for position, batch in enumerate(order):
                    label = f"{Path(name).stem}-{rate}-q{alternative}-block{block}-pos{position}"
                    env = os.environ.copy()
                    env.update(W09_STREAM_FIXTURES=str(cohort / "fixtures"), W09_BENCH_FIXTURE=name,
                               W09_BENCH_RATE=str(rate), W09_BENCH_CHANNELS=str(oracle["channels"]),
                               W09_BENCH_FRAMES=str(oracle["frames"]), W09_BENCH_BATCH=str(batch),
                               W09_BENCH_REPEATS=str(repeats))
                    command = ["/usr/bin/time", "-f", "%U %S %M", "-o", str(output / f"{label}.time"),
                               str(binary), "--exact", "transport_contracts::playback::pipeline_benchmark",
                               "--ignored", "--nocapture"]
                    with (output / f"{label}.log").open("w") as log:
                        status = subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT,
                                                timeout=90, preexec_fn=lambda: os.sched_setaffinity(0, {cpu})).returncode
                    assert status == 0, (label, status)
                    rows = marked((output / f"{label}.log").read_text(), "W09_PIPELINE ")
                    assert len(rows) == repeats
                    for row in rows:
                        value = row["measurement"]
                        assert value["pcm_hash"] == oracle["pcm_hash"] and value["frames"] == oracle["frames"], (label, row)
                    user, system, peak = (output / f"{label}.time").read_text().split()
                    record = {"fixture":name, "rate":rate, "alternative":alternative, "block":block,
                              "position":position, "batch":batch, "runs":rows,
                              "process_user_s":float(user), "process_system_s":float(system), "peak_rss_kib":int(peak)}
                    raw.append(record)
                print(f"{name}/{rate} batch{alternative} block{block+1}/{blocks} passed", flush=True)
    (output / "raw.json").write_text(json.dumps(raw, indent=2) + "\n")
    metrics = ["wall_ns", "cpu_ns", "first_pcm_ns", "steps", "io_thread_rust_allocations", "player_peak_pcm_capacity_bytes"]
    comparisons = []
    for name, rate in selected:
        for alternative in alternatives:
            group = [row for row in raw if row["fixture"] == name and row["rate"] == rate and row["alternative"] == alternative]
            row = {"fixture":name, "rate":rate, "base_batch":base_batch,"alternative":alternative, "phases":{}}
            for phase in ["cold", "warm"]:
                result = {}
                for metric in metrics:
                    def value(item):
                        runs = item["runs"][:1] if phase == "cold" else item["runs"][1:]
                        return statistics.median(run["measurement"][metric] for run in runs)
                    ratios = []
                    for block in range(blocks):
                        base = statistics.median(value(item) for item in group if item["block"] == block and item["batch"] == base_batch)
                        changed = statistics.median(value(item) for item in group if item["block"] == block and item["batch"] == alternative)
                        ratios.append(changed / base)
                    result[metric] = {"block_ratio_median":statistics.median(ratios), "block_ratio_range":[min(ratios),max(ratios)],
                                      "base_median":statistics.median(value(item) for item in group if item["batch"] == base_batch),
                                      "changed_median":statistics.median(value(item) for item in group if item["batch"] == alternative)}
                row["phases"][phase] = result
            comparisons.append(row)
    assert sha(binary) == environment["binary_sha256"], "retained positive binary changed during measurement"
    result = {"fresh_processes":len(raw), "playbacks":len(raw)*repeats, "binary_sha256":sha(binary),
              "same_pcm_all_arms":True, "comparisons":comparisons, "scope":__doc__}
    (output / "summary.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({key:result[key] for key in ["fresh_processes","playbacks","binary_sha256","same_pcm_all_arms"]}), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cohort", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--blocks", type=int, default=4)
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--base-batch", type=int, choices=[1,4,8,16], default=1)
    parser.add_argument("--alternatives", type=int, nargs="+", choices=[1,4,8,16], default=[4,8,16])
    args = parser.parse_args()
    main(args.cohort.resolve(), args.output.resolve(), args.blocks, args.repeats, args.base_batch, args.alternatives)
