#!/usr/bin/env bash
# Supply-chain policy is executable release behavior, not prose.
#
# Fixtures keep this gate load-bearing: mutable Actions, vulnerabilities,
# unsound advisories, expired exceptions, unknown licenses, git dependencies,
# and an unbound SBOM must each turn a green fixture red.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

failures=0
pass() { printf '\033[0;32m[ok]\033[0m %s\n' "$*"; }
fail() {
    printf '\033[0;31m[FAIL]\033[0m %s\n' "$*" >&2
    failures=$((failures + 1))
}

expect_pass() {
    local what="$1"
    shift
    local output status
    set +e
    output="$("$@" 2>&1)"
    status=$?
    set -e
    if [[ $status -eq 0 ]]; then
        pass "$what"
    else
        fail "$what (exit $status)"
        printf '%s\n' "$output" >&2
    fi
}

expect_fail() {
    local what="$1"
    shift
    local output status
    set +e
    output="$("$@" 2>&1)"
    status=$?
    set -e
    if [[ $status -ne 0 ]]; then
        pass "$what"
    else
        fail "$what unexpectedly passed"
        printf '%s\n' "$output" >&2
    fi
}

CHECKER="$ROOT/scripts/check-supply-chain.py"
SBOM="$ROOT/scripts/generate-sbom.py"

[[ -f "$CHECKER" ]] || fail "supply-chain checker exists"
[[ -f "$SBOM" ]] || fail "artifact-bound SBOM generator exists"

mkdir -p "$WORK/actions" "$WORK/scripts/ci"
cat > "$WORK/actions/pinned.yml" <<'EOF'
jobs:
  pinned:
    steps:
      - uses: actions/checkout@fbc6f3992d24b796d5a048ff273f7fcc4a7b6c09 # v5
      - uses: ./.github/actions/local
      - run: cargo install cargo-audit --version 0.22.2 --locked
      - run: >-
          python -m pip install --no-deps --only-binary=:all: --require-hashes
          --requirement scripts/ci/requirements.txt
EOF
cat > "$WORK/scripts/ci/requirements.txt" <<'EOF'
PyYAML==6.0.3 \
  --hash=sha256:b8bb0864c5a28024fac8a632c443c87c5aa6f215c0b126c449ae1a150412f31d
EOF
cat > "$WORK/actions/mutable.yml" <<'EOF'
jobs:
  mutable:
    steps:
      - uses: actions/checkout@v5
EOF
cat > "$WORK/actions/mutable-pip.yml" <<'EOF'
jobs:
  mutable:
    steps:
      - run: pip install pyyaml
EOF
cat > "$WORK/actions/mutable-cargo.yml" <<'EOF'
jobs:
  mutable:
    steps:
      - run: cargo install cargo-audit
EOF
cat > "$WORK/actions/unhashed-requirement.yml" <<'EOF'
jobs:
  mutable:
    steps:
      - run: >-
          python -m pip install --no-deps --only-binary=:all: --require-hashes
          --requirement scripts/ci/unhashed.txt
EOF
printf 'PyYAML==6.0.3\n' > "$WORK/scripts/ci/unhashed.txt"

mkdir -p "$WORK/actions-spaced-use" "$WORK/actions-spaced-run"
cat > "$WORK/actions-spaced-use/workflow.yml" <<'EOF'
jobs:
  bypass:
    steps:
      - uses: actions/checkout@fbc6f3992d24b796d5a048ff273f7fcc4a7b6c09
      - uses : actions/checkout@v5
EOF
cat > "$WORK/actions-spaced-run/workflow.yml" <<'EOF'
jobs:
  bypass:
    steps:
      - uses: actions/checkout@fbc6f3992d24b796d5a048ff273f7fcc4a7b6c09
      - run : pip install pyyaml
EOF

if [[ -f "$CHECKER" ]]; then
    expect_pass "immutable and local Actions are accepted" \
        python3 "$CHECKER" actions --workflows-dir "$WORK/actions" \
        --exclude mutable.yml --exclude mutable-pip.yml \
        --exclude mutable-cargo.yml --exclude unhashed-requirement.yml
    expect_fail "a mutable Action tag is rejected" \
        python3 "$CHECKER" actions --workflows-dir "$WORK/actions" \
        --exclude mutable-pip.yml --exclude mutable-cargo.yml \
        --exclude unhashed-requirement.yml
    expect_fail "an unpinned pip install is rejected" \
        python3 "$CHECKER" actions --workflows-dir "$WORK/actions" \
        --exclude mutable.yml --exclude mutable-cargo.yml \
        --exclude unhashed-requirement.yml
    expect_fail "an unpinned cargo install is rejected" \
        python3 "$CHECKER" actions --workflows-dir "$WORK/actions" \
        --exclude mutable.yml --exclude mutable-pip.yml \
        --exclude unhashed-requirement.yml
    expect_fail "a requirement without a SHA-256 is rejected" \
        python3 "$CHECKER" actions --workflows-dir "$WORK/actions" \
        --exclude mutable.yml --exclude mutable-pip.yml \
        --exclude mutable-cargo.yml
    expect_fail "YAML whitespace cannot hide a mutable Action" \
        python3 "$CHECKER" actions --workflows-dir "$WORK/actions-spaced-use"
    expect_fail "YAML whitespace cannot hide an unpinned installer" \
        python3 "$CHECKER" actions --workflows-dir "$WORK/actions-spaced-run"
fi

mkdir -p "$WORK/crates/root" "$WORK/crates/dep"
printf '[package]\nname="fixture-root"\nversion="1.0.0"\n' \
    > "$WORK/crates/root/Cargo.toml"
printf '[package]\nname="fixture-dep"\nversion="2.0.0"\nlicense="MIT"\n' \
    > "$WORK/crates/dep/Cargo.toml"
printf 'artifact bytes\n' > "$WORK/artifact.bin"

cat > "$WORK/metadata.json" <<EOF
{
  "packages": [
    {
      "id": "path+file://fixture/root#fixture-root@1.0.0",
      "name": "fixture-root",
      "version": "1.0.0",
      "source": null,
      "license": "BSL-1.1",
      "license_file": null,
      "manifest_path": "$WORK/crates/root/Cargo.toml"
    },
    {
      "id": "registry+https://github.com/rust-lang/crates.io-index#fixture-dep@2.0.0",
      "name": "fixture-dep",
      "version": "2.0.0",
      "source": "registry+https://github.com/rust-lang/crates.io-index",
      "license": "MIT",
      "license_file": null,
      "manifest_path": "$WORK/crates/dep/Cargo.toml",
      "description": "fixture",
      "repository": "https://example.invalid/fixture"
    }
  ],
  "workspace_members": ["path+file://fixture/root#fixture-root@1.0.0"],
  "resolve": {
    "nodes": [
      {
        "id": "path+file://fixture/root#fixture-root@1.0.0",
        "dependencies": ["registry+https://github.com/rust-lang/crates.io-index#fixture-dep@2.0.0"],
        "deps": []
      },
      {
        "id": "registry+https://github.com/rust-lang/crates.io-index#fixture-dep@2.0.0",
        "dependencies": [],
        "deps": []
      }
    ]
  }
}
EOF

cat > "$WORK/policy.toml" <<'EOF'
schema = 1

[licenses]
allowed = ["BSL-1.1", "MIT"]

[[advisory_exceptions]]
id = "RUSTSEC-2025-0141"
package = "legacy"
version = "1.0.0"
kind = "unmaintained"
expires = "2027-01-01"
reason = "Pinned by the fixture upstream until its replacement release."
tracking = "https://example.invalid/upstream/1"
EOF

cat > "$WORK/audit-clean.json" <<'EOF'
{
  "vulnerabilities": {"count": 0, "list": []},
  "warnings": {
    "unmaintained": [
      {
        "kind": "unmaintained",
        "package": {"name": "legacy", "version": "1.0.0"},
        "advisory": {"id": "RUSTSEC-2025-0141"}
      }
    ]
  }
}
EOF

if [[ -f "$CHECKER" ]]; then
    expect_pass "an exact, unexpired unmaintained exception is accepted" \
        python3 "$CHECKER" audit --audit-json "$WORK/audit-clean.json" \
        --metadata-json "$WORK/metadata.json" --policy "$WORK/policy.toml" \
        --workspace-root "$WORK" --as-of 2026-08-26
    expect_fail "the same exception fails closed after expiry" \
        python3 "$CHECKER" audit --audit-json "$WORK/audit-clean.json" \
        --metadata-json "$WORK/metadata.json" --policy "$WORK/policy.toml" \
        --workspace-root "$WORK" --as-of 2027-01-02

    python3 - "$WORK/audit-clean.json" "$WORK/audit-vulnerable.json" <<'PY'
import json, pathlib, sys
data = json.loads(pathlib.Path(sys.argv[1]).read_text())
data["vulnerabilities"] = {
    "count": 1,
    "list": [{"advisory": {"id": "RUSTSEC-2099-0001"},
              "package": {"name": "bad", "version": "1.0.0"}}],
}
pathlib.Path(sys.argv[2]).write_text(json.dumps(data))
PY
    expect_fail "a vulnerability cannot be excepted by warning policy" \
        python3 "$CHECKER" audit --audit-json "$WORK/audit-vulnerable.json" \
        --metadata-json "$WORK/metadata.json" --policy "$WORK/policy.toml" \
        --workspace-root "$WORK" --as-of 2026-08-26

    python3 - "$WORK/audit-clean.json" "$WORK/audit-unsound.json" <<'PY'
import json, pathlib, sys
data = json.loads(pathlib.Path(sys.argv[1]).read_text())
data["warnings"]["unsound"] = [{
    "kind": "unsound",
    "package": {"name": "legacy", "version": "1.0.0"},
    "advisory": {"id": "RUSTSEC-2025-0141"},
}]
pathlib.Path(sys.argv[2]).write_text(json.dumps(data))
PY
    expect_fail "unsound code is denied even if an unmaintained exception matches" \
        python3 "$CHECKER" audit --audit-json "$WORK/audit-unsound.json" \
        --metadata-json "$WORK/metadata.json" --policy "$WORK/policy.toml" \
        --workspace-root "$WORK" --as-of 2026-08-26

    # A "yanked" warning carries `"advisory": null`, not an absent key -- a
    # yanked release has no RUSTSEC id to report. cargo-audit produces this
    # shape for real (RUSTSEC-2025-0141's own exception list did not cover
    # it; a live audit against a yanked chacha20 0.10.1 crashed with
    # `AttributeError: 'NoneType' object has no attribute 'get'` before this
    # test existed). The gate must fail closed on the policy violation, not
    # crash on the shape.
    python3 - "$WORK/audit-clean.json" "$WORK/audit-yanked.json" <<'PY'
import json, pathlib, sys
data = json.loads(pathlib.Path(sys.argv[1]).read_text())
data["warnings"]["yanked"] = [{
    "kind": "yanked",
    "package": {"name": "chacha20", "version": "0.10.1"},
    "advisory": None,
}]
pathlib.Path(sys.argv[2]).write_text(json.dumps(data))
PY
    yanked_output="$(python3 "$CHECKER" audit --audit-json "$WORK/audit-yanked.json" \
        --metadata-json "$WORK/metadata.json" --policy "$WORK/policy.toml" \
        --workspace-root "$WORK" --as-of 2026-08-26 2>&1)" && yanked_status=0 || yanked_status=$?
    if [[ "$yanked_status" -eq 0 ]]; then
        fail "a yanked crate with no policy exception unexpectedly passed"
    elif grep -q "Traceback" <<<"$yanked_output"; then
        fail "a null-advisory yanked warning crashed the checker instead of failing closed"
        printf '%s\n' "$yanked_output" >&2
    elif ! grep -qi "yanked" <<<"$yanked_output"; then
        fail "the checker rejected the yanked warning for the wrong reason"
        printf '%s\n' "$yanked_output" >&2
    else
        pass "a null-advisory yanked warning fails closed, not with a traceback"
    fi

    python3 - "$WORK/metadata.json" "$WORK/metadata-git.json" <<'PY'
import json, pathlib, sys
data = json.loads(pathlib.Path(sys.argv[1]).read_text())
data["packages"][1]["source"] = "git+https://example.invalid/repo#mutable"
pathlib.Path(sys.argv[2]).write_text(json.dumps(data))
PY
    expect_fail "git dependencies are rejected" \
        python3 "$CHECKER" audit --audit-json "$WORK/audit-clean.json" \
        --metadata-json "$WORK/metadata-git.json" --policy "$WORK/policy.toml" \
        --workspace-root "$WORK" --as-of 2026-08-26

    python3 - "$WORK/metadata.json" "$WORK/metadata-registry.json" <<'PY'
import json, pathlib, sys
data = json.loads(pathlib.Path(sys.argv[1]).read_text())
data["packages"][1]["source"] = "registry+https://packages.example.invalid/index"
pathlib.Path(sys.argv[2]).write_text(json.dumps(data))
PY
    expect_fail "unapproved Cargo registries are rejected" \
        python3 "$CHECKER" audit --audit-json "$WORK/audit-clean.json" \
        --metadata-json "$WORK/metadata-registry.json" --policy "$WORK/policy.toml" \
        --workspace-root "$WORK" --as-of 2026-08-26

    python3 - "$WORK/metadata.json" "$WORK/metadata-unknown.json" <<'PY'
import json, pathlib, sys
data = json.loads(pathlib.Path(sys.argv[1]).read_text())
data["packages"][1]["license"] = None
pathlib.Path(sys.argv[2]).write_text(json.dumps(data))
PY
    expect_fail "an unknown dependency license is rejected" \
        python3 "$CHECKER" audit --audit-json "$WORK/audit-clean.json" \
        --metadata-json "$WORK/metadata-unknown.json" --policy "$WORK/policy.toml" \
        --workspace-root "$WORK" --as-of 2026-08-26
fi

if [[ -f "$SBOM" ]]; then
    expect_pass "SBOM generation accepts a licensed resolved graph" \
        python3 "$SBOM" --metadata "$WORK/metadata.json" \
        --artifact "$WORK/artifact.bin" --artifact-kind android-aar \
        --target aarch64-linux-android --profile full \
        --root-package fixture-root --policy "$WORK/policy.toml" \
        --workspace-root "$WORK" \
        --source-revision 0123456789abcdef --out "$WORK/sbom.json"

    if [[ -f "$WORK/sbom.json" ]]; then
        if python3 - "$WORK/sbom.json" "$WORK/artifact.bin" "$WORK" <<'PY'
import hashlib, json, pathlib, sys
sbom = json.loads(pathlib.Path(sys.argv[1]).read_text())
artifact = pathlib.Path(sys.argv[2])
work = str(pathlib.Path(sys.argv[3]).resolve())
component = sbom["metadata"]["component"]
hashes = {item["alg"]: item["content"] for item in component["hashes"]}
assert hashes["SHA-256"] == hashlib.sha256(artifact.read_bytes()).hexdigest()
props = {item["name"]: item["value"] for item in sbom["metadata"]["properties"]}
assert props["migo:artifact-kind"] == "android-aar"
assert props["migo:target"] == "aarch64-linux-android"
assert props["migo:profile"] == "full"
assert {item["name"] for item in sbom["components"]} == {"fixture-dep"}
assert work not in pathlib.Path(sys.argv[1]).read_text()
PY
        then
            pass "SBOM binds artifact hash/target/profile without leaking local paths"
        else
            fail "SBOM artifact binding or path-redaction assertion"
        fi
    fi

    # Feature unification. `cargo metadata` resolves the whole workspace's
    # features at once, so a crate another member enables is an edge here too
    # -- which is how the iOS SDK, built without V8 so it links no JavaScript
    # engine, would have listed `v8`. The build's own `cargo tree` is what
    # decides, and a second build's graph (the Apple SDK carries two) is a
    # union, not a replacement.
    python3 - "$WORK/metadata.json" "$WORK/metadata-unified.json" "$WORK/metadata-second.json" <<'PY'
import copy, json, pathlib, sys
base = json.loads(pathlib.Path(sys.argv[1]).read_text())
def with_dep(name):
    data = copy.deepcopy(base)
    dep = copy.deepcopy(data["packages"][1])
    dep["id"] = dep["id"].replace("fixture-dep@2.0.0", f"{name}@1.0.0")
    dep["name"], dep["version"] = name, "1.0.0"
    data["packages"].append(dep)
    data["resolve"]["nodes"][0]["dependencies"].append(dep["id"])
    data["resolve"]["nodes"].append({"id": dep["id"], "dependencies": [], "deps": []})
    return data
pathlib.Path(sys.argv[2]).write_text(json.dumps(with_dep("fixture-unified")))
# The second build does not compile fixture-dep at all, so only a union of
# the two graphs can still list it.
second = with_dep("fixture-second")
dep_id = second["packages"][1]["id"]
second["packages"] = [p for p in second["packages"] if p["id"] != dep_id]
second["resolve"]["nodes"] = [n for n in second["resolve"]["nodes"] if n["id"] != dep_id]
second["resolve"]["nodes"][0]["dependencies"].remove(dep_id)
pathlib.Path(sys.argv[3]).write_text(json.dumps(second))
PY
    printf 'fixture-root v1.0.0 (%s)\nfixture-dep v2.0.0\n' "$WORK" > "$WORK/tree-first.txt"
    printf 'fixture-root v1.0.0 (%s)\nfixture-second v1.0.0\n' "$WORK" > "$WORK/tree-second.txt"
    : > "$WORK/tree-empty.txt"
    sbom_names() {
        python3 -c 'import json,sys; print(",".join(sorted(c["name"] for c in json.load(open(sys.argv[1]))["components"])))' "$1"
    }
    sbom_run() {
        local out="$1"; shift
        python3 "$SBOM" "$@" --artifact "$WORK/artifact.bin" --artifact-kind apple-sdk \
            --target apple --profile full --root-package fixture-root --policy "$WORK/policy.toml" \
            --workspace-root "$WORK" --source-revision 0123456789abcdef --out "$out" >/dev/null
    }
    if sbom_run "$WORK/sbom-unified.json" --metadata "$WORK/metadata-unified.json" \
        --compiled "$WORK/tree-first.txt" \
        && [[ "$(sbom_names "$WORK/sbom-unified.json")" == "fixture-dep" ]]; then
        pass "SBOM drops a crate the workspace resolves and this build does not compile"
    else
        fail "SBOM listed a crate only feature unification put in the graph"
    fi
    if sbom_run "$WORK/sbom-union.json" --metadata "$WORK/metadata-unified.json" \
        --metadata "$WORK/metadata-second.json" \
        --compiled "$WORK/tree-first.txt" --compiled "$WORK/tree-second.txt" \
        && [[ "$(sbom_names "$WORK/sbom-union.json")" == "fixture-dep,fixture-second" ]]; then
        pass "SBOM of an artifact carrying two builds lists the union of what each compiles"
    else
        fail "SBOM of a two-build artifact is not the union of the builds"
    fi
    expect_fail "SBOM refuses an empty compiled-package list rather than listing nothing" \
        sbom_run "$WORK/sbom-empty.json" --metadata "$WORK/metadata.json" \
        --compiled "$WORK/tree-empty.txt"
fi

mkdir -p "$WORK/gradle/gradle/wrapper" "$WORK/gradle/gradle"
printf 'fixture wrapper\n' > "$WORK/gradle/gradle/wrapper/gradle-wrapper.jar"
wrapper_hash="$(sha256sum "$WORK/gradle/gradle/wrapper/gradle-wrapper.jar" | cut -d' ' -f1)"
cat > "$WORK/gradle/gradle/wrapper/gradle-wrapper.properties" <<'EOF'
distributionUrl=https\://services.gradle.org/distributions/gradle-8.4-bin.zip
distributionSha256Sum=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
EOF
cat > "$WORK/gradle/build.gradle" <<'EOF'
allprojects {
    dependencyLocking {
        lockAllConfigurations()
        lockMode = org.gradle.api.artifacts.dsl.LockMode.STRICT
    }
}
dependencies { testImplementation 'junit:junit:4.13.2' }
EOF
cat > "$WORK/gradle/gradle.lockfile" <<'EOF'
junit:junit:4.13.2=testRuntimeClasspath
empty=
EOF
cat > "$WORK/gradle/gradle/verification-metadata.xml" <<'EOF'
<verification-metadata xmlns="https://schema.gradle.org/dependency-verification">
  <configuration><verify-metadata>true</verify-metadata><verify-signatures>false</verify-signatures></configuration>
  <components>
    <component group="junit" name="junit" version="4.13.2">
      <artifact name="junit-4.13.2.jar"><sha256 value="bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"/></artifact>
    </component>
  </components>
</verification-metadata>
EOF
cat > "$WORK/policy-gradle.toml" <<EOF
schema = 1
[licenses]
allowed = ["MIT"]
[gradle]
distribution_url = "https://services.gradle.org/distributions/gradle-8.4-bin.zip"
distribution_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
wrapper_jar_sha256 = "$wrapper_hash"
EOF

if [[ -f "$CHECKER" ]]; then
    expect_pass "strict locked and checksum-verified Gradle input is accepted" \
        python3 "$CHECKER" gradle --project-dir "$WORK/gradle" \
        --policy "$WORK/policy-gradle.toml"
    cp -R "$WORK/gradle" "$WORK/gradle-mutable"
    printf "dependencies { testImplementation 'bad:bad:1.+' }\n" \
        >> "$WORK/gradle-mutable/build.gradle"
    expect_fail "a dynamic Gradle dependency is rejected" \
        python3 "$CHECKER" gradle --project-dir "$WORK/gradle-mutable" \
        --policy "$WORK/policy-gradle.toml"
    cp -R "$WORK/gradle" "$WORK/gradle-unlocked"
    rm "$WORK/gradle-unlocked/gradle.lockfile"
    expect_fail "missing Gradle lock state is rejected" \
        python3 "$CHECKER" gradle --project-dir "$WORK/gradle-unlocked" \
        --policy "$WORK/policy-gradle.toml"
fi

if (( failures > 0 )); then
    printf 'Supply-chain contract: FAIL (%d assertion(s))\n' "$failures" >&2
    exit 1
fi

echo "Supply-chain contract: PASS"
