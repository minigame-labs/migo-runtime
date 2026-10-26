"""Compare the exact package versions/features printed by cargo tree."""
import re
import sys

ROW = re.compile(r"^([\w-]+) v([^ |]+)(?: [^|]*)?\|([^|]*)$")


def parse(text):
    result = {}
    for line in text.splitlines():
        if not line.strip():
            continue
        match = ROW.fullmatch(line.removesuffix(" (*)"))
        if not match:
            raise ValueError(f"unexpected Cargo product row: {line!r}")
        name, version, features = match.groups()
        result.setdefault((name, version), set()).update(filter(None, features.split(",")))
    if not result:
        raise ValueError("Cargo product graph is empty")
    return result


def compare(left, right):
    left, right = parse(left), parse(right)
    differences = []
    for identity in sorted(left.keys() | right.keys()):
        name, version = identity
        if identity not in right:
            detail = f"v{version} only in the Apple resolution"
        elif identity not in left:
            detail = f"v{version} only in the reference resolution"
        elif left[identity] != right[identity]:
            detail = (f"v{version} features differ: apple-only={sorted(left[identity] - right[identity])} "
                      f"reference-only={sorted(right[identity] - left[identity])}")
        else:
            continue
        differences.append((name, detail))
    return differences


if __name__ == "__main__":
    if sys.argv[1] == "count":
        print(len(parse(sys.argv[2])))
    elif sys.argv[1] == "compare":
        for name, detail in compare(sys.argv[2], sys.argv[3]):
            print(f"{name}\t{detail}")
    else:
        raise ValueError("expected count or compare")
