#!/usr/bin/env bash
# Instructional docs must name release artifacts with the `<version>`
# placeholder, never a concrete version that goes stale on the next release.
#
# `release/VERSION` is the single source of the release version and
# `test-release-version-contract.sh` proves every *build consumer* derives from
# it -- but a hand-written version string in a README or BUILD.md is not a build
# consumer, so nothing kept those in step. They drifted: the Android quick-start
# told a stranger to copy `implementation files('libs/migo-0.9.3-android.aar')`
# while the release page carried `migo-0.9.4-android.aar`, a 404 on the first
# line of integration. BUILD.md's packaging examples still said `0.9.1`.
#
# The fix is not to bump them each release -- that is the checklist this repo
# removes everywhere else. `CHANGELOG.md` and `README.md` already use
# `migo-<version>-...`; this gate makes that the rule for every instructional
# document. The release documentation contract requires README, build
# documentation, and platform documentation to describe the same version.
#
# Scope: tracked Markdown and Gradle/Groovy/Kotlin files, excluding the
# changelog (its release headers and historical entries name concrete versions
# on purpose) and the licence/notice files.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

# `migo-1.2.3-...`, `libmigo.so.1.2.3`, `migo.dll` companions, etc. A bare
# `1.2.3` is not matched: version numbers appear in unrelated prose (NDK
# revisions, SemVer links) and only an artifact *name* carrying one decays.
pattern='(migo-[0-9]+\.[0-9]+\.[0-9]+|libmigo\.(so|a|dll|lib)\.[0-9]+\.[0-9]+\.[0-9]+)'

mapfile -t offenders < <(
    git grep -nE "$pattern" -- \
        '*.md' '*.gradle' '*.groovy' '*.kts' \
        ':!CHANGELOG.md' ':!**/CHANGELOG.md' \
        ':!LICENSE*' ':!NOTICE*' ':!**/LICENSE*' ':!**/NOTICE*' \
        2>/dev/null || true
)

if [[ ${#offenders[@]} -gt 0 ]]; then
    echo "ERROR: instructional docs name a release artifact with a concrete version." >&2
    echo "Use the '<version>' placeholder, as CHANGELOG.md and README.md already do:" >&2
    printf '  %s\n' "${offenders[@]}" >&2
    exit 1
fi

scanned=$(git grep -lE '(migo-<version>|libmigo\.so\.<version>)' -- '*.md' 2>/dev/null | wc -l)
echo "Doc version-reference contract: PASS (no concrete artifact versions in instructional docs; ${scanned} file(s) use the <version> placeholder)"
