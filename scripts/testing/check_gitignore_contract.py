"""Source and generated-output boundaries for current and earlier work."""
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts/lib'))
from gitignore_policy import check, tracked_ignored

REQUIRED = [
    'engine/Cargo.lock', 'tools/artifact-manifest/Cargo.lock',
    'engine/third_party/rustls-platform-verifier-android-0.2.0/classes.jar',
    'engine/third_party/rustls-platform-verifier-android-0.2.0/rustls-platform-verifier-0.2.0.aar',
    'engine/crates/runtime-v8/snapshots/SNAPSHOT-full-linux-x86_64.bin',
    'contracts/android-runtime-dependencies.json',
    'developer-docs/src/content/docs/reference/session.mdx',
    'developer-docs/public/brand/mark.svg',
    'engine/crates/io/src/polling.rs', 'scripts/lib/gitignore_policy.py',
]
GENERATED = [
    'engine/target/debug/deps/runtime_v8', 'tools/artifact-manifest/target/release/artifact-manifest',
    'engine/jniLibs/full/arm64-v8a/libmigo.so',
    'engine/third_party/rusty_v8/x86_64-linux-gnu/librusty_v8.a',
    'engine/third_party/rusty_v8/x86_64-linux-gnu/gn_out/obj/librusty_v8.a',
    'engine/third_party/rusty_v8/aarch64-linux-gnu/librusty_v8.a',
    'engine/third_party/rusty_v8/x86_64-pc-windows-msvc/rusty_v8.lib',
    'platforms/android/library/build/outputs/aar/migo-full-debug.aar',
    'platforms/apple/Frameworks/MigoEngine.xcframework/Info.plist',
    'platforms/apple/.build/debug/objects.o', 'platforms/android/.gradle/cache.bin',
    'developer-docs/build/index.html', 'developer-docs/.astro/content.d.ts',
    'developer-docs/playwright-report/index.html', 'developer-docs/test-results/trace.zip',
    'scripts/lib/__pycache__/polling.cpython-312.pyc',
    'docs/implementation/2026-10-25-runtime-modernization/state/loop-state.json',
    'docs/implementation/2026-10-25-runtime-modernization/results/session.log',
    'dist/maven/migo.aar', 'local-certificate.pem',
    'tests/c_host/android-package-consumer/build/CMakeCache.txt',
    'perf.data', 'perf.data.old', 'profile.cpuprofile', 'profile.heapsnapshot',
    'scripts/patch-helper.py.orig', 'scripts/patch-helper.py.rej',
]


def main():
    required = REQUIRED.copy()
    # The provenance gates check every path from their verified archive + patch
    # inventory. This fast gate checks the known broad-pattern exceptions.
    for name in ['deno_core-0.413.0', 'v8-150.4.0', 'ada-url-4.0.0', 'skia-bindings-0.93.1']:
        required.extend(f'engine/third_party/{name}/{file}'
                        for file in ['Cargo.lock', 'Cargo.toml.orig'])
    for name in required:
        if not (ROOT / name).is_file():
            raise ValueError(f'missing required source input: {name}')
    check(ROOT, required, GENERATED)
    # .gitignore alone does not stop a previously tracked/force-added report.
    # Source archives lack Git metadata; source boundaries still apply there.
    if (ROOT / '.git').exists():
        ignored = tracked_ignored(ROOT)
        if ignored:
            raise ValueError('ignored files still tracked; preserve local files and remove from index:\n'
                             + '\n'.join(ignored))
    print(f'Git ignore policy PASS: {len(required)} source paths / {len(GENERATED)} generated samples; main Git metadata untouched')


if __name__ == '__main__':
    main()
