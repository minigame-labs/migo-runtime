#!/usr/bin/env python3
"""The C ABI entry points one product exports, one name per line, sorted.

Usage: c-abi-entry-points.py <embedded|external-frames> <include-dir> [extra-header ...]

Read from the public headers -- the documented surface, which is what every
export allowlist and export check derives from -- with two rules the plain grep
they replaced did not have:

  * Comments are removed first. A header that mentions another header's entry
    point in prose ("available from migo_session_copy_content_root
    (external_frames.h)") was otherwise declaring it.
  * `external_frames.h` belongs to the external-frames product alone. Its
    entry points are `crates/capi/src/external_frames.rs`, compiled only with
    that feature; the embedded products (Android, Linux, Windows, OpenHarmony)
    do not have them. Asked of every header, the Windows export allowlist
    named 21 functions its DLL could not resolve, and the first release after
    the header landed failed at the link.

`extra-header` adds a platform header that declares entry points of its own
(Android's `platform/android.h`).
"""

import pathlib
import re
import sys

PRODUCTS = {"embedded", "external-frames"}
EXTERNAL_FRAMES_ONLY = "external_frames.h"
COMMENTS = re.compile(r"/\*.*?\*/|//[^\n]*", re.S)
ENTRY_POINT = re.compile(r"\b(migo_[a-z0-9_]+)\s*\(")


def entry_points(product: str, include_dir: pathlib.Path, extra: list[pathlib.Path]) -> list[str]:
    headers = sorted(include_dir.glob("*.h"))
    if product == "embedded":
        headers = [header for header in headers if header.name != EXTERNAL_FRAMES_ONLY]
    names: set[str] = set()
    for header in headers + extra:
        names.update(ENTRY_POINT.findall(COMMENTS.sub(" ", header.read_text(encoding="utf-8"))))
    return sorted(names)


def main(argv: list[str]) -> int:
    if len(argv) < 3 or argv[1] not in PRODUCTS:
        print(__doc__.split("\n\n")[1], file=sys.stderr)
        return 2
    include_dir = pathlib.Path(argv[2])
    if not (include_dir / "migo.h").is_file():
        print(f"c-abi-entry-points: no migo.h in {include_dir}", file=sys.stderr)
        return 2
    names = entry_points(argv[1], include_dir, [pathlib.Path(path) for path in argv[3:]])
    sys.stdout.write("".join(f"{name}\n" for name in names))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
