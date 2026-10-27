#!/usr/bin/env python3
"""Compile the real HTTP range reader and prove its guards reject faults.

Byte-range indexes need identity/coordinate/body checks even when the HTTP
exchange itself succeeds. Keep every source mutation in an isolated services
copy; compilation errors and deadlines are never accepted as red controls.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("range_transport_helpers", ROOT / "scripts/testing/check_audio_streaming_transport.py")
transport = importlib.util.module_from_spec(spec)
spec.loader.exec_module(transport)
codecs = transport.codecs


def build(work: Path, env: dict, label: str) -> Path:
    package = work / "probe"
    codecs.run(["cargo", "clean", "--offline", "--locked", "--release", "-p", "migo-range-source-probe"],
               work / f"{label}-clean.log", cwd=package, env=env)
    log = work / f"{label}-build.jsonl"
    codecs.run(["cargo", "test", "--offline", "--locked", "--release", "--lib", "--no-run", "--message-format=json"],
               log, cwd=package, env=env)
    rows = []
    for line in log.read_text().splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get("reason") == "compiler-artifact" and row.get("executable") and row.get("profile", {}).get("test"):
            assert not row["fresh"], "mutated test artifact must actually rebuild"
            rows.append(row)
    assert len(rows) == 1
    binary = work / f"{label}-tests"
    shutil.copy2(rows[0]["executable"], binary)
    return binary


def controls(work: Path, env: dict) -> int:
    path = work / "probe/src/network/http/range.rs"
    original = path.read_bytes()
    source = original.decode()
    bad = "actual_http_identity_extent_and_body_failures_never_return_partial_data"
    budget = "actual_http_read_byte_request_and_object_budgets_are_independent"
    declarations = [
        ("decoded-initial-response-accepted", 'if response.has_decoded_representation() {\n            return Err(Error::InvalidResponse("initial representation was decoded"));', 'if false {\n            return Err(Error::InvalidResponse("initial representation was decoded"));',
         "decoded_initial_response_cannot_be_pinned_even_after_its_headers_are_normalized"),
        ("weak-validator-accepted", "bytes.len() < 2", "false && bytes.len() < 2",
         "capture_requires_a_strong_validator_and_identity_encoding"),
        ("final-url-unchecked", "response.url().as_str() != self.identity.url.as_str()", "false", bad),
        ("etag-unchecked", "tag.is_some_and(|tag| tag != self.identity.etag)", "false", bad),
        ("content-encoding-unchecked", "        identity_encoding(response.headers())?;\n        let tag = single(response.headers(), ETAG.as_str())?;", "        let tag = single(response.headers(), ETAG.as_str())?;", bad),
        ("range-start-unchecked", "extent.start != start || extent.end >= end", "extent.end >= end", bad),
        ("range-end-unchecked", "extent.start != start || extent.end >= end", "extent.start != start", bad),
        ("object-length-unchecked", ".is_some_and(|(a, b)| a != b)", ".is_some_and(|_| false)", bad),
        ("duplicate-header-unchecked", "if values.next().is_some() {", "if false {", bad),
        ("truncated-body-accepted", "if self.remaining != 0 {", "if false {", bad),
        ("oversized-body-accepted", "if (chunk.len() as u64) > self.remaining {", "if false {", bad),
        ("read-cap-unchecked", "len > self.limits.max_read_bytes", "false", budget),
        ("total-byte-cap-unchecked", ".is_none_or(|n| n > self.limits.max_total_bytes)", ".is_none_or(|_| false)", budget),
        ("request-cap-unchecked", ".filter(|n| *n <= self.limits.max_requests)", ".filter(|_| true)", budget),
        ("failure-and-cancellation-not-sticky", "        self.failed = true;", "        self.failed = false;", bad),
        ("deadline-renewed", ".timeout(remaining)", ".timeout(std::time::Duration::from_secs(2))",
         "cancellation_and_one_absolute_deadline_end_pending_headers_body_and_partial_retries"),
        ("if-range-omitted", "            .header(IF_RANGE, self.identity.etag.clone())\n", "",
         "actual_http_fixed_chunked_partial_and_unknown_lengths_keep_exact_bytes"),
        ("remaining-body-length-as-object-length", 'length: single(response.headers(), CONTENT_LENGTH.as_str())?\n                .map(|n| {\n                    decimal(n.as_bytes()).ok_or(Error::InvalidResponse("invalid object length"))\n                })\n                .transpose()?',
         "length: response.content_length()", "initial_identity_length_does_not_change_after_body_is_consumed"),
    ]
    # Replace the complete strong-validator condition rather than changing its
    # first term (the remaining grammar terms would still reject a weak tag).
    first = source.index("        if bytes.len() < 2")
    end = source.index("            return Err(Error::NoStrongValidator);", first)
    grammar = source[first:end]
    declarations[1] = (declarations[1][0], grammar, "        if false {\n", declarations[1][3])
    for name, before, after, test in declarations:
        assert source.count(before) == 1, (name, "mutation anchor changed")
        path.write_text(source.replace(before, after))
        try:
            binary = build(work, env, name)
            log = work / f"{name}-test.log"
            with log.open("w") as stream:
                status = subprocess.run([str(binary), "--exact", "network::http::range::tests::" + test, "--nocapture"],
                                        cwd=work / "probe", env=env, stdout=stream, stderr=subprocess.STDOUT,
                                        timeout=45).returncode
            observed = log.read_text()
            assert status == 101 and "panicked at" in observed and "1 failed" in observed, (name, "must reject at an executed Rust assertion")
        finally:
            path.write_bytes(original)
        print("actual HTTP range mutation", name, ": executable assertion rejected", flush=True)
    return len(declarations)


def stream_controls(work: Path, env: dict) -> int:
    path = work / "probe/src/network/http/range/stream.rs"
    original = path.read_bytes()
    source = original.decode()
    short = "media_short_responses_continue_exactly_and_unknown_total_is_not_object_eof"
    broken = "media_rejects_headers_body_faults_and_version_changes_before_retry_or_success"
    timers = "media_header_idle_and_cancel_deadlines_are_separate_from_duration"
    budget = "media_request_count_extent_and_timeout_limits_fail_without_unbounded_requests"
    declarations = [
        ("media-request-cap-removed", ".filter(|n| *n <= self.limits.max_requests)", ".filter(|_| true)", budget),
        ("media-cancel-not-sticky", "        self.reader.failed = true;", "        self.reader.failed = false;", timers),
        ("media-error-not-sticky", "        if result.is_ok() {", "        if true {", broken),
        ("media-whole-exchange-timeout", ".open_range(self.position, self.end, deadline, false)", ".open_range(self.position, self.end, deadline, true)", timers),
        ("media-header-timeout-removed", ".checked_add(self.limits.header_timeout)", ".checked_add(Duration::from_secs(2))", timers),
        ("media-idle-timeout-removed", ".checked_add(self.limits.idle_timeout)", ".checked_add(Duration::from_secs(2))", timers),
        ("media-eof-before-response-end", "        loop {", "        loop {\n            if self.position == self.end { return Ok(None); }", broken),
        ("media-short-range-overlap", ".open_range(self.position, self.end, deadline, false)", ".open_range(self.position.saturating_sub(1), self.end, deadline, false)", short),
        ("media-short-range-as-eof", "                self.segment = None;", "                return Ok(None);", short),
        ("media-uses-control-byte-budget", "                    self.position += chunk.len() as u64;", "                    if self.bytes + chunk.len() as u64 > self.reader.limits.max_total_bytes { return Err(Error::Limit); }\n                    self.position += chunk.len() as u64;", "media_stream_exceeds_control_budgets_without_collecting_or_truncating_positions"),
        ("media-known-length-cap-removed", "self.identity.length.is_some_and(|n| end > n)", "false", budget),
    ]
    for name, before, after, test in declarations:
        assert source.count(before) == 1, (name, "mutation anchor changed")
        path.write_text(source.replace(before, after))
        try:
            binary = build(work, env, name)
            log = work / f"{name}-test.log"
            with log.open("w") as stream:
                status = subprocess.run([str(binary), "--exact", "network::http::range::tests::" + test, "--nocapture"],
                                        cwd=work / "probe", env=env, stdout=stream, stderr=subprocess.STDOUT,
                                        timeout=45).returncode
            observed = log.read_text()
            assert status == 101 and "panicked at" in observed and "1 failed" in observed, (name, "must reject at an executed Rust assertion")
        finally:
            path.write_bytes(original)
        print("actual HTTP media mutation", name, ": executable assertion rejected", flush=True)
    return len(declarations)


def prepare(work: Path, target: Path) -> tuple[dict, dict, dict, dict]:
    assert not work.exists(), "use a fresh HTTP range result directory"
    work.mkdir(parents=True)
    package = work / "probe"
    package.mkdir()
    services = ROOT / "engine/crates/services"
    shutil.copytree(services / "src", package / "src")
    manifest = transport.absolute_paths((services / "Cargo.toml").read_text(), services)
    manifest = manifest.replace('name = "migo-services"', 'name = "migo-range-source-probe"', 1)
    root_manifest = (ROOT / "engine/Cargo.toml").read_text()
    workspace = root_manifest.split('[workspace.dependencies]\n', 1)[1].split('\n[', 1)[0]
    manifest += '\n[workspace]\n[workspace.package]\nversion="0.0.0"\nlicense="BSL-1.1"\n'
    manifest += '\n[workspace.dependencies]\n' + transport.absolute_paths(workspace, ROOT / "engine")
    manifest += '\n[profile.release]\nopt-level=3\n[patch.crates-io]\n'
    for name, relative in [("ada-url", "ada-url-4.0.0"), ("tungstenite", "tungstenite-0.30.0")]:
        manifest += f'{name}={{path={json.dumps(str(ROOT / "engine/third_party" / relative))}}}\n'
    (package / "Cargo.toml").write_text(manifest)
    (package / "Cargo.lock").write_bytes((ROOT / "engine/Cargo.lock").read_bytes())
    inputs = [Path(__file__).resolve(), ROOT / "scripts/testing/check_audio_streaming_transport.py",
              ROOT / "scripts/testing/check_audio_codec_timeline.py", ROOT / "scripts/bench/audio-codecs.py",
              ROOT / "engine/Cargo.toml", ROOT / "engine/Cargo.lock"]
    for name in ["services", "io", "shared"]:
        inputs += sorted((ROOT / f"engine/crates/{name}/src").rglob("*.rs"))
        inputs.append(ROOT / f"engine/crates/{name}/Cargo.toml")
    frozen = {str(p.relative_to(ROOT)): codecs.sha(p) for p in inputs}
    (work / "source-inputs.json").write_text(json.dumps(frozen, indent=2) + "\n")
    retained = {str(p.relative_to(package)): codecs.sha(p) for p in (package / "src").rglob("*.rs")}
    env = os.environ.copy()
    env.update(RUSTUP_TOOLCHAIN="1.95.0", CARGO_TARGET_DIR=str(target), CARGO_NET_OFFLINE="true")
    env.pop("ANDROID_NDK", None)
    env.pop("ANDROID_NDK_HOME", None)
    codecs.run(["cargo", "metadata", "--offline", "--format-version", "1"], work / "metadata.jsonl", cwd=package, env=env)
    selected = json.loads(next(line for line in (work / "metadata.jsonl").read_text().splitlines() if line.startswith('{"packages"')))
    locked = tomllib.loads((ROOT / "engine/Cargo.lock").read_text())["package"]
    identities = {(p["name"],p["version"],p.get("source")) for p in locked}
    for row in selected["packages"]:
        if row["name"] != "migo-range-source-probe":
            assert (row["name"],row["version"],row.get("source")) in identities, (row["name"], "unplanned dependency")
    return env, frozen, retained, selected


def main(work: Path, target: Path) -> None:
    env, frozen, retained, selected = prepare(work, target)
    package = work / "probe"
    positive = build(work, env, "positive")
    codecs.run([str(positive), "network::http::range::tests::", "--nocapture", "--test-threads=1"],
               work / "positive-tests.log", cwd=package, env=env)
    assert "16 passed; 0 failed" in (work / "positive-tests.log").read_text()
    negative = controls(work, env)
    media_negative = stream_controls(work, env)
    assert all(codecs.sha(ROOT / path) == checksum for path,checksum in frozen.items()), "Root inputs changed"
    assert all(codecs.sha(package / path) == checksum for path,checksum in retained.items()), "service copy not restored"
    summary = {"positive_tests":16,"compiled_controls":negative,"compiled_media_controls":media_negative,"binary_sha256":codecs.sha(positive),
               "selected_identities":len(selected["packages"]),"new_dependency_identities":0,
               "source_copy_restored":True,"root_inputs_unchanged":True,"root_lock_sha256":codecs.sha(ROOT / "engine/Cargo.lock"),
               "scope":"Real host loopback control/media HTTP range source: 6 MiB actual media at virtual >16 GiB position, without whole-object collection; no M4A transport/DSP/player/fallback or native-device/performance acceptance"}
    (work / "summary.json").write_text(json.dumps(summary,indent=2)+"\n")
    print(json.dumps(summary,indent=2),flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output",type=Path,required=True)
    parser.add_argument("--target-dir",type=Path,required=True)
    args=parser.parse_args()
    main(args.output.resolve(),args.target_dir.resolve())
