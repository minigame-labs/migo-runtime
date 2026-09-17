#!/usr/bin/env python3
"""The engine's JavaScript module graph, read out of its sources.

deno_core evaluates each extension's `esm_entry_point` in the order the runtime
registers extensions, then `ext:runtime/99_main.js`, and resolves every
`ext:<extension>/<file>` specifier against that extension's `esm = [dir ...]`
list. A runtime that is not deno_core -- the Performance+ producer in WebKit's
WebContent -- has to evaluate the same modules in the same order, so this
derives both rather than restating them:

  order        the extension sequence of `snapshot.rs::lazy_extensions()`,
               which the embedded runtime's own snapshot is built from
  modules      `ext:<extension>/<file>` -> source path, from each extension's
               esm list
  entries      each extension's `esm_entry_point`, in order, then the
               runtime's
  closure      every module reachable from the entries through static imports

Standard library only. Used by scripts/gen-performance-plus-engine.py.
"""

from __future__ import annotations

import pathlib
import re
import sys
from dataclasses import dataclass, field

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import runtime_ops  # noqa: E402

RUNTIME_SRC = runtime_ops.RUNTIME_SRC
RUNTIME_CRATE = RUNTIME_SRC.parent
SNAPSHOT = RUNTIME_SRC / "snapshot.rs"
MAIN_ENTRY = "ext:runtime/99_main.js"

STATIC_IMPORT = re.compile(
    r"""(?m)^\s*(?:import|export)\s[^;'"]*?\bfrom\s*(['"])(?P<spec>[^'"]+)\1"""
    r"""|^\s*import\s*(['"])(?P<bare>[^'"]+)\3"""
)


@dataclass
class Extension:
    name: str
    source: pathlib.Path
    esm: dict[str, pathlib.Path] = field(default_factory=dict)
    entry: str | None = None


def _strings(text: str) -> list[str]:
    return re.findall(r'"([^"]*)"', text)


def extensions(root: pathlib.Path) -> dict[str, Extension]:
    found: dict[str, Extension] = {}
    for path in sorted((root / RUNTIME_SRC).rglob("*.rs")):
        if runtime_ops.is_test_source(path.relative_to(root / RUNTIME_SRC)):
            continue
        original = path.read_text(encoding="utf-8")
        masked = runtime_ops.mask_rust(original)
        excluded = runtime_ops.cfg_test_spans(masked)
        for match in runtime_ops.EXTENSION_OPEN.finditer(masked):
            if any(start <= match.start() < end for start, end in excluded):
                continue
            open_index = match.end() - 1
            body_masked = runtime_ops.balanced(masked, open_index)
            body = original[open_index + 1 : open_index + 1 + len(body_masked)]
            name_match = runtime_ops.IDENT.search(body_masked)
            if not name_match:
                continue
            extension = Extension(name=name_match.group(0), source=path)
            entry = re.search(r"\besm_entry_point\s*=\s*\"([^\"]+)\"", body)
            if entry:
                extension.entry = entry.group(1)
            esm_open = re.search(r"\besm\s*=\s*\[", body_masked)
            if esm_open:
                listed = body[esm_open.end() : esm_open.end() + len(runtime_ops.balanced(body_masked, esm_open.end() - 1))]
                directory = re.search(r"\bdir\s+\"([^\"]+)\"", listed)
                if directory is None:
                    raise ValueError(f"{path}: {extension.name} lists esm without a dir")
                base = root / RUNTIME_CRATE / directory.group(1)
                names = _strings(listed[directory.end() :])
                for name in names:
                    extension.esm[f"ext:{extension.name}/{name}"] = base / name
            if extension.name in found:
                raise ValueError(f"extension {extension.name} is declared twice")
            found[extension.name] = extension
    return found


def extension_order(root: pathlib.Path, known: dict[str, Extension]) -> list[str]:
    """The registration sequence of `lazy_extensions()`, expanded through the
    per-module factory functions it calls, depth first, in call order."""
    src = root / RUNTIME_SRC
    snapshot = runtime_ops.mask_rust((root / SNAPSHOT).read_text(encoding="utf-8"))
    start = snapshot.index("pub fn lazy_extensions()")
    body = runtime_ops.balanced(snapshot, snapshot.index("{", start))

    # (module directory, function name) -> body. The directory is what a
    # `module::factory()` call names; the same function name can exist in two
    # modules (snapshot.rs and worker/ both define `worker_lazy_extensions`).
    factories: dict[tuple[str, str], str] = {}
    for path in sorted(src.rglob("*.rs")):
        if runtime_ops.is_test_source(path.relative_to(src)):
            continue
        text = runtime_ops.mask_rust(path.read_text(encoding="utf-8"))
        module = str(path.parent.relative_to(src)) if path.name == "mod.rs" or path.parent != src else ""
        for match in re.finditer(
            r"pub(?:\([^)]*\))?\s+fn\s+(\w+_lazy_extensions)\s*\(\)\s*->\s*Vec<[\w:]*Extension>\s*\{", text
        ):
            factories[(module, match.group(1))] = runtime_ops.balanced(text, match.end() - 1)

    call = re.compile(r"(?P<path>(?:\w+::)*)(?P<name>\w+)::lazy_init\(\)|(?P<fpath>(?:\w+::)*)(?P<factory>\w+_lazy_extensions)\(\)")

    def expand(text: str, module: str, depth: int) -> list[str]:
        if depth > 8:
            raise ValueError("extension factories nest deeper than any real module layout")
        names: list[str] = []
        for match in call.finditer(text):
            if match.group("name"):
                names.append(match.group("name"))
                continue
            qualifier = [
                part for part in match.group("fpath").split("::") if part and part not in ("super", "crate", "self")
            ]
            # `image::f()` inside rendering/ names rendering/image; `worker::f()`
            # at the top names worker/. A child module of the caller wins, as in
            # Rust's own path resolution from inside that module.
            if not qualifier:
                target = module
            elif module and (src / module / qualifier[0]).is_dir():
                target = "/".join([module, *qualifier])
            else:
                target = "/".join(qualifier)
            key = (target, match.group("factory"))
            if key not in factories:
                raise ValueError(f"{match.group(0)} names no factory in {target or 'src'}")
            names.extend(expand(factories[key], target, depth + 1))
        return names

    order: list[str] = []
    for name in expand(body, "", 0):
        if name not in known:
            raise ValueError(f"lazy_extensions() registers {name}, which no extension! declares")
        if name not in order:
            order.append(name)
    if not order or order[-1] != "runtime":
        raise ValueError("lazy_extensions() must register `runtime` last; the order read is wrong")
    return order


def module_map(known: dict[str, Extension]) -> dict[str, pathlib.Path]:
    modules: dict[str, pathlib.Path] = {}
    for extension in known.values():
        for specifier, path in extension.esm.items():
            modules[specifier] = path
    return modules


def imports_of(path: pathlib.Path) -> list[str]:
    text = runtime_ops.strip_js_comments(path.read_text(encoding="utf-8"))
    return [match.group("spec") or match.group("bare") for match in STATIC_IMPORT.finditer(text)]


def resolve(specifier: str, importer: str) -> str:
    """An import as an `ext:` specifier. Relative imports resolve against the
    importer's own extension directory."""
    if specifier.startswith("ext:"):
        return specifier
    if specifier.startswith("./"):
        return importer.rsplit("/", 1)[0] + "/" + specifier[2:]
    raise ValueError(f"{importer} imports {specifier!r}, which is neither ext: nor ./")


def graph(root: pathlib.Path) -> tuple[list[str], list[str], dict[str, pathlib.Path], dict[str, list[str]]]:
    """(entries in evaluation order, reachable modules, specifier -> path,
    specifier -> resolved imports)."""
    known = extensions(root)
    order = extension_order(root, known)
    entries = [known[name].entry for name in order if known[name].entry and name != "runtime"]
    entries.append(MAIN_ENTRY)
    modules = module_map(known)
    edges: dict[str, list[str]] = {}
    pending = list(entries)
    while pending:
        specifier = pending.pop()
        if specifier in edges or specifier.startswith("ext:core/"):
            continue
        path = modules.get(specifier)
        if path is None or not path.is_file():
            raise ValueError(f"{specifier} is imported and no extension's esm list provides it")
        resolved = [resolve(item, specifier) for item in imports_of(path)]
        edges[specifier] = resolved
        pending.extend(resolved)
    return entries, sorted(edges), modules, edges


if __name__ == "__main__":
    entries, reachable, _, _ = graph(pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "."))
    print("entries:")
    for entry in entries:
        print("  " + entry)
    print(f"{len(reachable)} modules reachable")
