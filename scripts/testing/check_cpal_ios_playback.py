"""Run the actual iOS playback policy and prove semantic regressions turn red.

Tests compile the production module, with native operations injected as closures.
The twelve mutations change executable policy/dispatch, not assertions.
No original source is edited, and no Apple SDK or device result is simulated.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys


def build_and_run(source: Path, hub: Path, integration: Path, work: Path) -> subprocess.CompletedProcess:
    work.mkdir(parents=True)
    harness = work / "harness.rs"
    harness.write_text(
        f'#[path = {json.dumps(str(source))}]\nmod playback_state;\n'
        f'#[path = {json.dumps(str(hub))}]\nmod interruption_hub;\n'
        + integration.read_text()
    )
    binary = work / "policy-tests"
    subprocess.run(["rustc", "--edition=2021", "--test", str(harness), "-o", str(binary)],
                   check=True)
    env = os.environ.copy()
    # An assertion abort must never create a core dump in the working tree.
    return subprocess.run([str(binary)], cwd=work, env=env, text=True,
                          stdout=subprocess.PIPE, stderr=subprocess.STDOUT)


def check(root: Path, work: Path) -> None:
    source = root / "engine/third_party/cpal-0.18.2/src/host/coreaudio/ios/playback_state.rs"
    original = source.read_text()
    hub = source.with_name("interruption_hub.rs")
    integration = root / "scripts/testing/fixtures/cpal/ios_interruption_integration.rs"
    result = build_and_run(source, hub, integration, work / "production")
    print(result.stdout, end="")
    assert result.returncode == 0, "production iOS playback policy failed"
    mutations = {
        "ignore-should-resume": ("if should_resume && self.state == State::Interrupted {",
                                 "if self.state == State::Interrupted {"),
        "pause-retains-auto-resume": ("self.state = State::Stopped;\n        Ok(())",
                                      "if !self.interruption_in_progress { self.state = State::Stopped; }\n        Ok(())"),
        "play-starts-during-interruption": ("if self.interruption_in_progress {",
                                            "if false {"),
        "denial-keeps-interruption-in-progress": ("self.interruption_in_progress = false;",
                                                  "self.interruption_in_progress = !should_resume;"),
        "failed-start-commits-playing": ("start()?;\n            self.state = State::Playing;",
                                         "self.state = State::Playing;\n            start()?;"),
        "idle-end-reactivates-session": ("if should_resume && self.state == State::Interrupted {\n            self.play(start)?;",
                                          "if should_resume {\n            self.play(start)?;"),
        "stale-end-overrides-denial": ("if !self.interruption_in_progress {\n            return Ok(());\n        }",
                                       "// incorrectly accept an end without an active interruption"),
    }
    for name, (before, after) in mutations.items():
        assert original.count(before) == 1, f"mutation {name} no longer matches production"
        mutated = work / f"{name}.rs"
        mutated.write_text(original.replace(before, after, 1))
        result = build_and_run(mutated, hub, integration, work / name)
        assert result.returncode != 0 and "FAILED" in result.stdout, \
            f"negative control {name} did not fail an executable assertion: {result.stdout}"
        (work / f"{name}.log").write_text(result.stdout)
        print(f"CPAL iOS executable negative control {name}: assertion failed as required")
    hub_mutations = {
        "public-before-native": ("streams.iter().chain(listeners.iter())", "listeners.iter().chain(streams.iter())"),
        "drop-loses-subscription-lifetime": ("InterruptionObserver { _handler: handler }", "InterruptionObserver { _handler: Arc::new(|_| {}) }"),
        "missing-native-dispatch": ("streams.iter().chain(listeners.iter())", "listeners.iter()"),
        "missing-public-dispatch": ("streams.iter().chain(listeners.iter())", "streams.iter()"),
        "wrong-process-interruption-state": ("event == InterruptionEvent::Began, Ordering::Release", "false, Ordering::Release"),
    }
    original_hub = hub.read_text()
    for name, (before, after) in hub_mutations.items():
        assert original_hub.count(before) == 1, f"hub mutation {name} no longer matches production"
        mutated = work / f"{name}.rs"
        mutated.write_text(original_hub.replace(before, after, 1))
        result = build_and_run(source, mutated, integration, work / name)
        assert result.returncode != 0 and "FAILED" in result.stdout, \
            f"negative control {name} did not fail an executable assertion: {result.stdout}"
        (work / f"{name}.log").write_text(result.stdout)
        print(f"CPAL iOS executable negative control {name}: assertion failed as required")
    print("CPAL iOS production playback/hub / twelve semantic negative controls PASS")


if __name__ == "__main__":
    check(Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve())
