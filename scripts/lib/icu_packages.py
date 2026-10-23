#!/usr/bin/env python3
"""Count the ICU common-data packages in a binary (a .so, a static archive, anything with bytes).

    python3 scripts/lib/icu_packages.py <file> [--expect N]

Every ICU carries its data as one common-data package, and every package starts with the same
header: `MappedData` (headerSize, magic 0xda 0x27) then `UDataInfo`, whose dataFormat four bytes in
read "CmnD". The items inside a package have headers too, but each names its own format (`ResB`,
`Brk `, `Dict`, ...), so the count of CmnD headers is the count of ICUs -- whatever their versions,
which a count of distinct `icudtNNl` names is not: two copies of one version share a name.

Prints the count and each package's name; with --expect, exits 1 unless the count matches.
"""
import re
import sys

HEADER = re.compile(rb"(?s).{2}\xda\x27.{8}CmnD")
NAME = re.compile(rb"icudt[0-9]{2}[lb]")


def packages(data):
    found = []
    for m in HEADER.finditer(data):
        # The package's own name is the prefix of its first table-of-contents entry, a few KB in.
        name = NAME.search(data, m.start(), m.start() + 65536)
        found.append(name.group(0).decode() if name else "?")
    return found


def main(argv):
    if len(argv) < 2:
        print(__doc__)
        return 2
    expect = int(argv[argv.index("--expect") + 1]) if "--expect" in argv else None
    with open(argv[1], "rb") as f:
        found = packages(f.read())
    print(f"{len(found)} {' '.join(found)}".strip())
    return 0 if expect is None or len(found) == expect else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
