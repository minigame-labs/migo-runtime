"""Verify that the URL conformance fixtures are unchanged pinned upstream data."""
import hashlib
import json
from pathlib import Path
import re

PARAM_CASES = ("constructor", "append", "delete", "get", "getall", "has", "set", "sort", "stringifier", "foreach")
FIXTURES = {f"urlsearchparams-{name}.any.js" for name in PARAM_CASES} | {
    "urltestdata.json", "setters_tests.json", "LICENSE.md",
}


def verify(directory):
    directory = Path(directory)
    provenance = json.loads((directory / "provenance.json").read_text())
    revision = provenance["revision"]
    if not re.fullmatch(r"[0-9a-f]{40}", revision) or provenance["repository"] != "https://github.com/web-platform-tests/wpt":
        raise ValueError("WPT repository/revision must be pinned")
    files = provenance["files"]
    if set(files) != FIXTURES:
        raise ValueError("review WPT fixture coverage when updating the manifest")
    for name, identity in files.items():
        subpath = name if name == "LICENSE.md" else "url/" + ("resources/" if name.endswith(".json") else "") + name
        expected_url = f"https://raw.githubusercontent.com/web-platform-tests/wpt/{revision}/{subpath}"
        if identity["url"] != expected_url:
            raise ValueError(f"fixture does not name the pinned upstream path: {name}")
        data = (directory / name).read_bytes()
        if len(data) != identity["bytes"] or hashlib.sha256(data).hexdigest() != identity["sha256"]:
            raise ValueError(f"WPT fixture differs from its upstream identity: {name}")
    constructors = json.loads((directory / "urltestdata.json").read_text())
    setters = json.loads((directory / "setters_tests.json").read_text())
    if sum(isinstance(row, dict) for row in constructors) != 896 or sum(len(rows) for name, rows in setters.items() if name != "comment") != 278:
        raise ValueError("review WPT constructor/setter coverage")
