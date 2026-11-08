#!/usr/bin/env bash
# scripts/test-skia-bindings-vendor-contract.sh
#
# Assert that engine/third_party/skia-bindings-<version> is the published crate plus the patches in
# engine/third_party/skia-bindings-patches, nothing else, and that Cargo actually builds it.
#
# WHY THIS EXISTS:
# skia-bindings is vendored so Skia can link V8's ICU instead of bundling a second one (see
# scripts/lib/skia-external-icu.sh). A vendored crate rots two quiet ways, and neither fails a build:
#
#   - an edit lands in the copy and not in a patch, so the next upgrade -- re-extract, re-apply --
#     drops it without a word, or a reviewer reads a patch that is not what ships;
#   - skia-safe moves to a skia-bindings version the copy is not, Cargo answers `[patch]` with a
#     "patch was not used" warning, and the registry crate builds with its bundled ICU again: the
#     binary grows 1.36 MB and every test still passes.
#
# WHY IT CHECKS WHAT IT CHECKS:
# The published .crate is pinned by sha256, extracted, patched, and compared with the copy byte for
# byte; Cargo.lock must name skia-bindings at that version with no registry source, which is the
# lockfile's own way of saying the patch is in effect. Then the comparison is run again against two
# broken copies (one byte changed, one patch withheld) and must fail on both: a comparison that
# passes a tampered copy is not a comparison.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
PATCH_DIR="$REPO_ROOT/engine/third_party/skia-bindings-patches"
TAG='[skia-bindings-vendor]'

pass() { echo -e "\033[0;32m$TAG PASS $*\033[0m"; }
fail() { echo -e "\033[0;31m$TAG FAIL $*\033[0m" >&2; failures=$((failures + 1)); }
failures=0

# Optional real SkUnicode/SkParagraph check, reusing an existing native Skia build.
# MIGO_SKIA_NATIVE_OUT, SKIA_SOURCE_DIR and MIGO_ICU_PROBE_OUT name the inputs/output.
if [[ "${1:-}" == --native-icu ]]; then
    [[ $# == 1 ]] || { echo "usage: $0 [--native-icu]" >&2; exit 2; }
    : "${MIGO_SKIA_NATIVE_OUT:?name the native Skia output directory}"
    : "${SKIA_SOURCE_DIR:?name the matching Skia source directory}"
    : "${MIGO_ICU_PROBE_OUT:?name a scratch directory for this probe}"
    source "$SCRIPT_DIR/lib/host-v8.sh"
    source "$SCRIPT_DIR/lib/skia-external-icu.sh"
    host_v8_resolve "$REPO_ROOT" || exit 1
    skia_external_icu_export "$REPO_ROOT/engine" x86_64-unknown-linux-gnu || exit 1
    python3 - "$REPO_ROOT" "$SKIA_SOURCE_DIR" "$MIGO_SKIA_NATIVE_OUT" "$MIGO_ICU_PROBE_OUT" "$HOST_V8_ARCHIVE" <<'NATIVE_ICU'
from pathlib import Path
import hashlib, json, os, re, shlex, subprocess, sys
root, skia, native, out, v8_archive = map(lambda p: Path(p).resolve(), sys.argv[1:])
out.mkdir(parents=True, exist_ok=True)

def run(argv, tag, cwd=None):
    result = subprocess.run([str(a) for a in argv], cwd=cwd, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    (out / (tag + '.log')).write_text(result.stdout)
    if result.returncode:
        raise RuntimeError(tag + ': ' + result.stdout[-2500:])
    return result.stdout

def digest(path):
    with path.open('rb') as file:
        return hashlib.file_digest(file, 'sha256').hexdigest()

metadata = json.loads(run(['cargo', 'metadata', '--manifest-path', root / 'engine/Cargo.toml',
                          '--locked', '--offline', '--format-version', '1',
                          '--filter-platform', 'x86_64-unknown-linux-gnu'], 'metadata'))
def package(name):
    matches = [p for p in metadata['packages'] if p['name'] == name]
    assert len(matches) == 1, name
    return Path(matches[0]['manifest_path']).parent
icu77 = package('v8') / 'third_party/icu/source'
full = package('deno_core_icudata') / 'src/icudtl.dat'
icu78 = skia / 'third_party/externals/icu/source'
for headers, major in [(icu77, 77), (icu78, 78)]:
    version = (headers / 'common/unicode/uvernum.h').read_text()
    assert re.search(r'#define\s+U_ICU_VERSION_MAJOR_NUM\s+' + str(major) + r'\b', version), headers
assert os.environ['SKIA_EXTERNAL_ICU_INCLUDE_DIRS'].split(os.pathsep) == [str(icu77 / 'common'), str(icu77 / 'i18n')]
for target in ['aarch64-apple-ios', 'aarch64-apple-ios-sim', 'x86_64-apple-ios']:
    check = subprocess.run(['/usr/bin/bash', '-c',
        'source "$1"; export SKIA_EXTERNAL_ICU_INCLUDE_DIRS=stale; '
        'if skia_external_icu_export "$2" "$3"; then exit 3; fi; '
        '[[ -z ${SKIA_EXTERNAL_ICU_INCLUDE_DIRS+x} ]]',
        'icu-guard', str(root / 'scripts/lib/skia-external-icu.sh'), str(root / 'engine'), target],
        env={'PATH': '/nonexistent'}, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    assert check.returncode == 0 and 'iOS uses standalone Skia ICU' in check.stdout, check.stdout
    (out / (target + '.log')).write_text(check.stdout)

# Use the existing Rust package reader and policy, not another data-format implementation.
generator = out / 'data_filter.rs'
generator.write_text(
    '#[path=' + json.dumps(str(root / 'engine/crates/icu-data/src/package.rs'), ensure_ascii=False) + '] mod package;\n'
    '#[path=' + json.dumps(str(root / 'engine/crates/icu-data/src/policy.rs'), ensure_ascii=False) + '] mod policy;\n'
    'fn main() { let a: Vec<_> = std::env::args().collect(); '
    'let data = std::fs::read(&a[1]).unwrap(); let p = package::Package::parse(&data).unwrap(); '
    'std::fs::write(&a[2], p.filtered(policy::keep)).unwrap(); '
    'std::fs::write(&a[3], p.filtered(|n| policy::keep(n) && !n.ends_with("/cjdict.dict"))).unwrap(); }\n')
run(['rustc', '--edition=2024', '-O', generator, '-o', out / 'data_filter'], 'data-filter-build')
filtered, missing_dict = out / 'filtered.dat', out / 'missing-dictionary.dat'
run([out / 'data_filter', full, filtered, missing_dict], 'data-filter')

commands = run([os.environ.get('SKIA_NINJA_COMMAND', 'ninja'), '-C', native,
                '-t', 'commands', 'libskunicode_icu.a'], 'native-commands')
compiled, objects = [], []
cxx = shlex.split(os.environ.get('CXX', 'clang++-18'))
for command in commands.splitlines():
    if ' -c ' not in command:
        continue
    args = shlex.split(command)
    if not Path(args[args.index('-o') + 1]).name.startswith('libskunicode_icu.'):
        continue
    source = (native / args[args.index('-c') + 1]).resolve()
    assert source.is_relative_to(skia), source
    obj = out / (source.stem + '.o')
    args[args.index('-o') + 1] = str(obj)
    args[args.index('-MF') + 1] = str(obj) + '.d'
    args = [a for a in args[1:] if a not in ('-DU_DISABLE_RENAMING=1', '-DSK_USING_THIRD_PARTY_ICU')
            and 'third_party/externals/icu' not in a]
    args = cxx + ['-I' + str(icu77 / 'common'), '-I' + str(icu77 / 'i18n')] + args
    run(args, source.stem, native)
    compiled.append(args)
    objects.append(obj)
assert len(objects) == 4, 'expected the four actual SkUnicode ICU adapter sources'

fixture = root / 'engine/crates/graphics/tests/fixtures/icu_compat.cpp'
libs = [native / ('lib' + n + '.a') for n in
        ['skparagraph', 'skshaper', 'skunicode_core', 'skia', 'harfbuzz',
         'freetype2', 'png', 'zlib', 'wuffs', 'expat', 'skcms']]
common = ['-std=c++20', '-O2', '-fno-rtti', '-I' + str(skia), fixture,
          '-Wl,--no-as-needed', '-lstdc++', '-Wl,--as-needed', '-Wl,--start-group']
tail = ['-Wl,--end-group', '-pthread', '-ldl', '-lrt', '-lm']
shared = cxx + ['-I' + str(icu77 / 'common'), '-I' + str(icu77 / 'i18n')] + common + objects + libs + [v8_archive] + tail + ['-o', out / 'shared77']
bundled = cxx + ['-DU_DISABLE_RENAMING=1', '-I' + str(icu78 / 'common'), '-I' + str(icu78 / 'i18n')] + common + libs + [native / 'libskunicode_icu.a'] + tail + ['-o', out / 'bundled78']
run(shared, 'shared-link')
run(bundled, 'bundled-link')
font = root / 'engine/crates/graphics/tests/fixtures/fonts/NotoSans-Regular.ttf'
for name, program, data in [('full77', out / 'shared77', full),
                            ('filtered77', out / 'shared77', filtered),
                            ('full78', out / 'bundled78', skia / 'third_party/externals/icu/common/icudtl.dat')]:
    run([program, data, font, out / (name + '.rgba')], name)
# Runtime version lines differ; compare only the semantic/layout output and actual pixels.
def body(name):
    return '\n'.join(line for line in (out / (name + '.log')).read_text().splitlines()
                     if not line.startswith('ICU '))
assert body('full77') == body('filtered77'), 'filtered data changes Skia text results'
assert (out / 'full77.rgba').read_bytes() == (out / 'filtered77.rgba').read_bytes(), 'filtered pixels differ'
assert body('full77') == body('full78'), 'selected fixture changed from bundled ICU 78'
assert (out / 'full77.rgba').read_bytes() == (out / 'full78.rgba').read_bytes(), 'bundled baseline pixels differ'

negative = []
for name, data, expected in [('missing-dictionary', missing_dict, 'Chinese dictionary segmentation'),
                              ('wrong-major', skia / 'third_party/externals/icu/common/icudtl.dat', 'break iterator')]:
    result = subprocess.run([str(out / 'shared77'), str(data), str(font), str(out / (name + '.rgba'))],
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    (out / (name + '.log')).write_text(result.stdout)
    assert result.returncode == 2 and expected in result.stdout, (name, result.returncode, result.stdout[-1000:])
    negative.append({'case': name, 'exit_code': result.returncode, 'specified_assertion': expected})
for name in ['shared77', 'bundled78']:
    symbols = run(['nm', '-C', '--defined-only', out / name], name + '-symbols')
    assert not re.search(r'\b[vV]8::|\bcppgc::', symbols), 'probe unexpectedly links a VM'

inputs = [fixture, root / 'engine/crates/icu-data/src/package.rs',
          root / 'engine/crates/icu-data/src/policy.rs', full, filtered, font, v8_archive, *libs,
          native / 'libskunicode_icu.a', icu77 / 'common/unicode/uvernum.h',
          icu78 / 'common/unicode/uvernum.h', skia / 'third_party/externals/icu/common/icudtl.dat',
          *[(native / args[args.index('-c') + 1]).resolve() for args in compiled]]
record = {'status': 'passed', 'break_cases': 24, 'paragraph_cases': 18, 'number_cases': 1,
          'ios_targets_rejected_before_tool_lookup': 3, 'stale_provider_cleared_on_rejection': True,
          'rgba_bytes_each': (out / 'full77.rgba').stat().st_size,
          'full_filtered_and_bundled_baseline_equal': True, 'negative_cases': negative,
          'both_native_probes_have_no_v8_or_cppgc_symbols': True,
          'native_programs': {n: {'sha256': digest(out / n), 'bytes': (out / n).stat().st_size}
                              for n in ['shared77', 'bundled78']},
          'compiled_adapter_commands': compiled, 'shared_link': [str(a) for a in shared],
          'bundled_link': [str(a) for a in bundled],
          'input_sha256': {str(p): digest(p) for p in inputs},
          'scope': 'Host native C ABI/Skia adapter, break/layout/pixel checks with an explicit Noto face. Unresolved glyph counts are recorded, not claimed as full multilingual glyph coverage. No iOS/native SDK artifact or device/performance result.'}
(out / 'native-result.json').write_text(json.dumps(record, indent=2) + '\n')
print('PASS: ICU 77 full/filtered and ICU 78 baseline; 24 break + 18 paragraph + 1 number cases, exact pixels, two specified negatives, no VM symbols.')
NATIVE_ICU
    exit "$?"
fi

read -r name version sha256 < "$PATCH_DIR/UPSTREAM" || { echo "$TAG cannot read $PATCH_DIR/UPSTREAM" >&2; exit 1; }
VENDORED="$REPO_ROOT/engine/third_party/$name-$version"
[[ -d "$VENDORED" ]] || { echo "$TAG no vendored crate at $VENDORED" >&2; exit 1; }

scratch="$(mktemp -d)" || { echo "$TAG no writable temp directory" >&2; exit 1; }
trap 'rm -rf "$scratch"' EXIT

crate="$(find "${CARGO_HOME:-$HOME/.cargo}/registry/cache" -maxdepth 2 -name "$name-$version.crate" 2>/dev/null | head -1)"
if [[ -z "$crate" ]]; then
    crate="$scratch/$name-$version.crate"
    curl -fsSL "https://static.crates.io/crates/$name/$name-$version.crate" -o "$crate" \
        || { echo "$TAG cannot fetch $name-$version.crate" >&2; exit 1; }
fi
actual="$(sha256sum "$crate" | cut -d' ' -f1)"
if [[ "$actual" == "$sha256" ]]; then
    pass "$name-$version.crate matches the pinned sha256"
else
    fail "$name-$version.crate is $actual, UPSTREAM pins $sha256"
fi

# <tree to compare> <patch list...>: 0 when pristine + patches equals the tree.
pristine_plus_patches_is() {
    local tree="$1" work
    shift
    work="$(mktemp -d -p "$scratch")"
    tar xzf "$crate" -C "$work" || return 1
    local p
    for p in "$@"; do
        (cd "$work" && patch -s -p0 --forward < "$p") >/dev/null || return 1
    done
    # Only the top-level skia/ and depot_tools/ are excluded: they are what the build script downloads
    # into the crate. `diff --exclude=skia` would also skip build_support/skia/, which is where the
    # patch lands -- the comparison would have passed any edit to the patched files.
    diff <(tree_manifest "$work/$name-$version") <(tree_manifest "$tree") >/dev/null
}

tree_manifest() { # <tree>: one "sha256  path" line per file, sorted.
    (cd "$1" && find . \( -path ./skia -o -path ./depot_tools \) -prune -o -type f -print0 \
        | sort -z | xargs -0 sha256sum)
}

mapfile -t patches < <(find "$PATCH_DIR" -maxdepth 1 -name '*.patch' | sort)
if ((${#patches[@]} == 0)); then
    fail "no patches in $PATCH_DIR"
elif pristine_plus_patches_is "$VENDORED" "${patches[@]}"; then
    pass "$name-$version + ${#patches[@]} patch(es) == the vendored copy"
else
    fail "the vendored copy is not $name-$version + the patches in $PATCH_DIR"
fi

lock_entry="$(awk -v n="$name" '/^\[\[package\]\]/{p=0} $0=="name = \""n"\""{p=1} p' "$REPO_ROOT/engine/Cargo.lock")"
if grep -q "^version = \"$version\"" <<<"$lock_entry" && ! grep -q '^source = ' <<<"$lock_entry"; then
    pass "Cargo.lock builds $name $version from the vendored path"
else
    fail "Cargo.lock does not build $name $version from the vendored path (is the [patch] unused?)"
fi

# The lockfile says a path crate is built; the manifest says which path. Pointing the [patch] at another
# copy of the same version would leave Cargo.lock as it is and this gate checking a directory nobody builds.
patched="$(python3 -c 'import sys, tomllib
m = tomllib.load(open(sys.argv[1], "rb"))
print(m.get("patch", {}).get("crates-io", {}).get(sys.argv[2], {}).get("path", ""))' "$REPO_ROOT/engine/Cargo.toml" "$name")"
if [[ -n "$patched" && "$(realpath -m "$REPO_ROOT/engine/$patched")" == "$(realpath -m "$VENDORED")" ]]; then
    pass "engine/Cargo.toml patches $name to the verified copy"
else
    fail "engine/Cargo.toml patches $name to '${patched:-nothing}', not $VENDORED"
fi

# The comparison must be able to fail.
tampered="$scratch/tampered"
# The edit goes into the file the patch itself changes, the one place an exclusion could hide it.
# The copy takes tree_manifest's file list: bsdtar's `--exclude=./skia` also drops
# build_support/skia/, and a copy missing the file under test is refused for the wrong reason.
mkdir -p "$tampered" && (cd "$VENDORED" && find . \( -path ./skia -o -path ./depot_tools \) -prune \
    -o -type f -print | tar cf - -T -) | (cd "$tampered" && tar xf -) \
    && printf '\n' >> "$tampered/build_support/skia/env.rs" \
    || { echo "$TAG cannot build the tampered copy" >&2; exit 1; }
if pristine_plus_patches_is "$tampered" "${patches[@]}"; then
    fail "self-test: a copy with an unpatched edit compared equal"
else
    pass "self-test: a copy with an unpatched edit is refused"
fi
if ((${#patches[@]} > 0)) && pristine_plus_patches_is "$VENDORED" "${patches[@]:1}"; then
    fail "self-test: the copy compared equal with a patch withheld"
else
    pass "self-test: the copy is refused with a patch withheld"
fi

# Execute the real GN argument generator without compiling native Skia. The
# both-on PDF feature concealed reversed JPEG flags; each direction must work
# independently, with bundled and system libraries.
mkdir -p "$scratch/features/src"
python3 - "$VENDORED" "$scratch/features" <<'PY' || exit 1
import io, json, pathlib, shutil, sys, tarfile, tomllib
vendor, work = map(pathlib.Path, sys.argv[1:])
manifest = tomllib.loads((vendor / 'Cargo.toml').read_text())
dependencies = manifest['build-dependencies']
text = '[package]\nname="skia-feature-contract"\nversion="0.0.0"\nedition="2021"\n[workspace]\n'
text += '[features]\n' + ''.join(json.dumps(name) + '=' +
    (json.dumps(manifest['features'][name]) if name == 'binary-cache' else '[]') + '\n'
    for name in manifest['features'])
for name, values in dependencies.items():
    text += '\n[dependencies.' + json.dumps(name) + ']\n'
    text += ''.join(json.dumps(key) + '=' + json.dumps(value) + '\n'
                    for key, value in values.items())
(work / 'Cargo.toml').write_text(text)
shutil.copy2(vendor / 'build_support.rs', work / 'src/build_support.rs')
shutil.copytree(vendor / 'build_support', work / 'src/build_support')
(work / '.cargo_vcs_info.json').write_text(json.dumps({'git': {'sha1': '0' * 40}}))
# A marker archive exercises the actual importer without downloading native code.
with tarfile.open(work / 'cache.tar.gz', 'w:gz') as archive:
    member = tarfile.TarInfo('skia-binaries/cache-marker')
    member.size = 1
    archive.addfile(member, io.BytesIO(b'x'))
(work / 'src/lib.rs').write_text('mod build_support;\n' + r'''
#[test]
fn jpeg_features_generate_independent_native_flags() {
    use build_support::{cargo, features::{feature, Features}, skia};
    for system in [false, true] {
        for (label, features, decode, encode, pdf) in [
            ("none", Features::default(), false, false, false),
            ("decode", Features::from([feature::JPEG_DECODE]), true, false, false),
            ("encode", Features::from([feature::JPEG_ENCODE]), false, true, false),
            ("both", Features::from([feature::JPEG_DECODE, feature::JPEG_ENCODE]), true, true, false),
            ("pdf", Features::from([feature::PDF, feature::JPEG_DECODE, feature::JPEG_ENCODE]), true, true, true),
        ] {
            let build = skia::BuildConfiguration {
                on_windows: false, opt_level: None, skia_debug: false,
                features, cc: "clang".into(), cxx: "clang++".into(),
                target: cargo::parse_target("x86_64-unknown-linux-gnu"),
            };
            let generated = skia::FinalBuildConfiguration::from_build_configuration(
                &build, system, std::path::Path::new("."));
            for (name, expected) in [
                ("skia_use_libjpeg_turbo_decode", decode),
                ("skia_use_libjpeg_turbo_encode", encode),
                ("skia_enable_pdf", pdf),
                ("skia_use_system_libjpeg_turbo", system),
            ] {
                let actual: Vec<_> = generated.gn_args.iter().filter(|(key, _)| key == name).collect();
                assert_eq!(actual.len(), 1, "{label} system={system}: {name} must occur once");
                assert_eq!(actual[0].1, expected.to_string(),
                           "{label} system={system}: {name} selects the wrong component");
            }
        }
    }
}

#[test]
fn jpeg_features_use_corrected_upstream_binaries() {
    use build_support::{binaries_config::BinariesConfiguration, features::{feature, Features}};
    for (label, features, rebuild) in [
        ("decode", Features::from([feature::JPEG_DECODE]), false),
        ("encode", Features::from([feature::JPEG_ENCODE]), false),
        ("none", Features::default(), false),
        ("both", Features::from([feature::JPEG_DECODE, feature::JPEG_ENCODE]), false),
    ] {
        let mut config = BinariesConfiguration::from_features(&features, false);
        config.output_directory.push(label);
        assert_eq!(build_support::binary_cache::try_prepare_download(&config), rebuild,
                   "{label}: unexpected upstream cache policy");
        assert_eq!(config.output_directory.join("cache-marker").is_file(), !rebuild,
                   "{label}: cache import must follow the selected feature policy");
    }
}

#[test]
fn jpeg_features_keep_the_external_icu_build_contract() {
    use build_support::{binaries_config::BinariesConfiguration, features::{feature, Features}, cargo, skia};
    let features = Features::from([feature::TEXTLAYOUT]);
    let mut config = BinariesConfiguration::from_features(&features, false);
    config.output_directory.push("external-icu");
    let build = skia::BuildConfiguration {
        on_windows: false, opt_level: None, skia_debug: false, features,
        cc: "clang".into(), cxx: "clang++".into(),
        target: cargo::parse_target("x86_64-unknown-linux-gnu"),
    };
    // The script invokes these tests serially and clears the inherited override.
    let headers = std::env::join_paths(["/icu/common", "/icu/i18n"]).unwrap();
    unsafe { std::env::set_var("SKIA_EXTERNAL_ICU_INCLUDE_DIRS", headers) };
    let generated = skia::FinalBuildConfiguration::from_build_configuration(
        &build, false, std::path::Path::new("."));
    let rebuild = build_support::binary_cache::try_prepare_download(&config);
    unsafe { std::env::remove_var("SKIA_EXTERNAL_ICU_INCLUDE_DIRS") };
    assert!(rebuild, "external ICU accepted a bundled-ICU prebuilt");
    assert!(!config.output_directory.join("cache-marker").exists());
    assert!(generated.gn_args.iter().any(|(k,v)| k == "skia_use_system_icu" && v == "true"));
    assert!(generated.gn_args.iter().any(|(k,v)| k == "extra_cflags" && v.contains("-I/icu/common") && v.contains("-I/icu/i18n")));
}

''')
PY
cp "$REPO_ROOT/engine/Cargo.lock" "$scratch/features/Cargo.lock" || exit 1
feature_host="$(rustc -vV | sed -n 's/^host: //p')"
if env -u FORCE_SKIA_BUILD -u FORCE_SKIA_BINARIES_DOWNLOAD -u SKIA_EXTERNAL_ICU_INCLUDE_DIRS \
    HOST="$feature_host" TARGET="$feature_host" OUT_DIR="$scratch/features/out" \
    SKIA_BINARIES_URL="file://$scratch/features/cache.tar.gz" \
    cargo test --manifest-path "$scratch/features/Cargo.toml" --features binary-cache \
    jpeg_features_ -- --nocapture --test-threads=1; then
    pass "real GN generator: 10 JPEG/PDF combinations; actual cache import: 4 feature sets + external ICU"
else
    fail "GN/cache/ICU build contract failed"
fi

((failures == 0)) || { echo "$TAG $failures failure(s)" >&2; exit 1; }
