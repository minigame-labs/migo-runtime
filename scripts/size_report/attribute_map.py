#!/usr/bin/env python3
"""Attribute libmigo.so's bytes to the components that put them there, from an ld.lld link map.

The shipped .so is stripped, so its symbols cannot say who owns what; the link map can. Produce one
with `MIGO_LINK_MAP=/tmp/libmigo.map scripts/build-android-so.sh arm64-v8a release` (the binary is
unchanged: the map is a side output of the same link; it lands at /tmp/libmigo.arm64-v8a.map), then:

    python3 scripts/size_report/attribute_map.py /tmp/libmigo.arm64-v8a.map [--detail 'V8 crate']

Two things about lld's map decide how it is read. Its symbols are demangled (`v8::internal::...`),
and V8's input sections are bare `.text`, so a section is classified by the first symbol the map
lists under it rather than by its name. And V8 and Skia arrive bundled inside rlibs
(`libv8-*.rlib(member.o)`, `libskia_bindings-*.rlib(libicu.x.o)`), not as `.a` files.
"""
import collections
import re
import sys

KEEP_OUT = {".text", ".rodata", ".data.rel.ro", ".data", ".eh_frame", ".gcc_except_table", ".bss", ".tbss",
            ".tdata", ".init_array"}

# Earliest-occurring namespace wins, so `v8::internal::maglev::X<std::__Cr::...>` is Maglev, not libc++.
NAMESPACES = [
    ("v8::internal::maglev::", "maglev"), ("v8::internal::compiler::turboshaft::", "turboshaft"),
    ("v8::internal::compiler::", "turbofan"), ("v8::internal::wasm::", "wasm"), ("v8::internal::Wasm", "wasm"),
    ("v8::internal::baseline::", "sparkplug"), ("v8::internal::interpreter::", "ignition"),
    ("v8::internal::Builtin_", "builtins (C++)"), ("v8::internal::Runtime_", "runtime functions"),
    ("v8::internal::", "v8 other"), ("v8_inspector::", "inspector"), ("v8::", "v8 api"),
    ("std::__Cr::", "libc++ (Chromium)"), ("std::__ndk1::", "libc++ (NDK)"),
    ("absl::", "abseil"), ("cppgc::", "heap/gc"), ("simdutf", "simdutf"), ("hwy::", "highway"),
]

SKIA_MEMBER_PREFIX = {
    "libicu": "ICU (Skia's own)", "libharfbuzz": "harfbuzz", "libfreetype2": "freetype",
    "typeface_freetype": "fonts", "libskunicode_icu": "skunicode", "libskunicode_core": "skunicode",
    "libskshaper": "skshaper", "libskparagraph": "skparagraph", "libpng": "libpng", "libwuffs": "wuffs",
    "libexpat": "expat", "core": "core", "gpu": "ganesh", "libskia": "sksl/other", "effects": "effects",
    "codec": "codecs", "shaders": "shaders", "image": "image", "pathops": "pathops", "utils": "utils",
    "text": "text", "ports": "ports", "opts": "opts", "pdf": "pdf", "encode": "encode",
}

C_ARCHIVES = [("liblibsqlite3_sys", "sqlite3"), ("libzstd_sys", "zstd"), ("liboboe_sys", "oboe"),
              ("libring", "ring"), ("libminimp3_sys", "minimp3")]

# ICU renames every symbol with its major version: `icu_77::UnicodeString` in C++, and every C entry
# point, public or not, by suffix: `u_strlen_77`, `T_CString_toLowerCase_77`. The version is read from
# the map (`icu_version`), so a bump of V8's ICU needs no edit here.
ICU_NAMESPACE = re.compile(r"icu_(\d+)::")
icu_entry_point = None
V0_CRATE = re.compile(r"C(?:s[0-9a-zA-Z]*_)?(\d+)([A-Za-z_][A-Za-z0-9_]*)")
LINE = re.compile(r"^\s*([0-9a-f]+)\s+([0-9a-f]+)\s+([0-9a-f]+)\s+(\d+)\s(.*)$")


def namespace_of(symbol):
    # (offset, list order): at one offset the more specific namespace, listed first, wins.
    found = [(symbol.find(token), n, name) for n, (token, name) in enumerate(NAMESPACES) if token in symbol]
    icu = ICU_NAMESPACE.search(symbol)
    if icu:
        found.append((icu.start(), len(NAMESPACES), "ICU code"))
    if found:
        return min(found)[2]
    return "ICU code" if icu_entry_point and icu_entry_point.search(symbol) else None


def icu_version(path):
    with open(path, errors="replace") as f:
        for line in f:
            m = ICU_NAMESPACE.search(line)
            if m:
                return m.group(1)
    return None


def rust_crate(symbol):
    """The crate of a Rust function section name, v0 (`_R...`) or legacy (`_ZN...`) mangled."""
    if symbol.startswith("_R"):
        m = V0_CRATE.search(symbol)
        return m.group(2)[: int(m.group(1))] if m else "rust (unparsed)"
    m = re.match(r"_ZN(\d+)", symbol)
    if m:
        ident = symbol[len(m.group(0)):][: int(m.group(1))]
        return ident[5:].split("..")[0].split("$")[0] if ident.startswith("_$LT$") else ident
    return None


def classify(path, section, first_symbol):
    named = section.split(".", 2)[2] if section.count(".") >= 2 else ""
    if named.startswith(".L"):
        named = ""
    symbol = first_symbol or named
    m = re.search(r"/([^/]+?)\(([^)]*)\)$", path)
    if m:
        archive, member = m.groups()
        if archive.startswith("libv8-"):
            if "embedded" in member:
                return "V8 crate", "embedded builtins"
            return "V8 crate", namespace_of(symbol) or "v8 other"
        if archive.startswith("libskia_bindings-"):
            ns = namespace_of(symbol)
            if ns and ns.startswith("libc++"):
                return "Skia crate", ns
            if re.match(r"^[0-9a-f]{16}-", member):
                return "Skia crate", "bindings glue"
            prefix = member.split(".", 1)[0]
            sub = SKIA_MEMBER_PREFIX.get(prefix, prefix)
            return "Skia crate", "sksl" if sub == "sksl/other" and "SkSL" in member else sub
        if archive == "libc++_static.a":
            return "NDK libc++", "libc++_static.a"
        for stem, name in C_ARCHIVES:
            if archive.startswith(stem):
                return "C/C++ sys crates", name
        if "clang_rt" in archive or archive.startswith("libcompiler_builtins"):
            return "compiler-rt", archive.split("-")[0]
        return "other archive", archive
    if path.startswith("<internal>"):
        return "linker internal", section
    if "rcgu.o" in path:
        crate = rust_crate(named) if named else None
        if crate:
            return "Rust", crate
        return "Rust", "(anonymous .rodata: JS sources, fonts, tables)" if section.startswith(".rodata") \
            else "(unnamed " + section + ")"
    return "other object", path.rsplit("/", 1)[-1]


def main(argv):
    if len(argv) < 2:
        print(__doc__)
        return 2
    detail = argv[argv.index("--detail") + 1] if "--detail" in argv else None
    global icu_entry_point
    version = icu_version(argv[1])
    icu_entry_point = re.compile(rf"_{version}(\(|$)") if version else None
    totals = collections.Counter()
    subs = collections.Counter()
    out_totals = collections.Counter()
    big = []
    current = None
    pending = None

    def finalize(p):
        size, out, path, section, first = p
        comp, sub = classify(path, section, first)
        key = out if out in (".text", ".rodata") else "other"
        totals[(comp, key)] += size
        subs[(comp, sub)] += size
        out_totals[key] += size
        if size >= 200_000:
            big.append((size, out, comp, sub, section[:60], path.rsplit("/", 1)[-1][:60]))

    with open(argv[1], errors="replace") as f:
        for line in f:
            m = LINE.match(line)
            if not m:
                continue
            size, rest = int(m.group(3), 16), m.group(5)
            if not rest.startswith(" "):
                if pending:
                    finalize(pending)
                    pending = None
                current = rest.strip()
                continue
            body = rest.strip()
            inp = re.match(r"(.+):\((\S+)\)$", body)
            if inp:
                if pending:
                    finalize(pending)
                    pending = None
                if current in KEEP_OUT:
                    pending = [size, current, inp.group(1), inp.group(2), None]
                continue
            if pending and pending[4] is None and not body.startswith(("$", ".L")):
                pending[4] = body
        if pending:
            finalize(pending)

    mb = 1 / 1048576
    comps = sorted({c for c, _ in totals}, key=lambda c: -sum(v for (cc, _), v in totals.items() if cc == c))
    print(f"{'component':24} {'.text':>7} {'.rodata':>7} {'other':>7} {'total':>7}  (MB)")
    for c in comps:
        t, r, o = (totals[(c, k)] for k in (".text", ".rodata", "other"))
        print(f"{c:24} {t * mb:7.2f} {r * mb:7.2f} {o * mb:7.2f} {(t + r + o) * mb:7.2f}")
    t, r, o = (out_totals[k] for k in (".text", ".rodata", "other"))
    print(f"{'sum':24} {t * mb:7.2f} {r * mb:7.2f} {o * mb:7.2f} {(t + r + o) * mb:7.2f}\n")
    for c in comps:
        if detail and c != detail:
            continue
        agg = collections.Counter({sub: v for (cc, sub), v in subs.items() if cc == c})
        shown = [f"{sub} {v * mb:.2f}" for sub, v in agg.most_common(None if detail else 12) if v * mb >= 0.04]
        print(f"[{c}] " + ", ".join(shown))
    print("\ninput sections of 200 KB or more:")
    for size, out, comp, sub, section, source in sorted(big, reverse=True)[:20]:
        print(f"  {size * mb:6.2f} {out:8} {comp:16} {sub:24} {section:40} {source}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
