#!/usr/bin/env bash
# Developer-docs phase 1 contract (Starlight edition).
#
# Holds together before any site build runs:
#   * the docs toolchain (Astro/Starlight) deps pinned against the lockfile
#   * release/VERSION gating via developer-docs/docs.config.mjs
#   * the zh route skeleton and its 4 sidebar groups
#   * en pages that are either a full translation or an explicit
#     TranslationPending stub (meta-noindexed, linking back to the zh page) --
#     never half of each; a translation links within the en tree
#   * the C ABI reference: one section per public header entry point, in zh and
#     in the en translation alike
#   * no Docusaurus remnants
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DOCS_DIR="$ROOT_DIR/developer-docs"

python3 - "$ROOT_DIR" "$DOCS_DIR" <<'PY'
from __future__ import annotations

import json
import pathlib
import re
import subprocess
import sys

root = pathlib.Path(sys.argv[1])
docs = pathlib.Path(sys.argv[2])
errors: list[str] = []


def error(message: str) -> None:
    errors.append(message)


def read_utf8(path: pathlib.Path, label: str) -> str | None:
    try:
        return path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        error(f"{label} cannot be read as UTF-8: {exc}")
        return None


def read_json(path: pathlib.Path, label: str) -> object | None:
    text = read_utf8(path, label)
    if text is None:
        return None
    try:
        return json.loads(text)
    except json.JSONDecodeError as exc:
        error(f"{label} is not valid JSON: {exc}")
        return None


# -- 1. toolchain pinning -----------------------------------------------------

package_json = read_json(docs / "package.json", "developer-docs/package.json")
package_lock = read_json(docs / "package-lock.json", "developer-docs/package-lock.json")

if isinstance(package_json, dict):
    engines = package_json.get("engines", {})
    if isinstance(engines, dict):
        node = engines.get("node")
        if node != ">=22.19.0":
            error(f"package.json engines.node must be '>=22.19.0', got {node!r}")
    deps = {**package_json.get("dependencies", {}), **package_json.get("devDependencies", {})}
    for name in ("@astrojs/starlight", "astro"):
        if name not in deps:
            error(f"package.json must depend on {name}")
    if any(name.startswith("@docusaurus") or name == "docusaurus" for name in deps):
        error("package.json must not depend on Docusaurus")

if isinstance(package_json, dict) and isinstance(package_lock, dict):
    lock_root = package_lock.get("packages", {}).get("")
    if not isinstance(lock_root, dict):
        error("package-lock.json has no root package record")
    else:
        for field in ("dependencies", "devDependencies"):
            pkg_field = package_json.get(field, {})
            lock_field = lock_root.get(field, {})
            if not isinstance(pkg_field, dict) or not isinstance(lock_field, dict):
                continue
            missing = [name for name in pkg_field if name not in lock_field and name in pkg_field]
            if pkg_field != lock_field:
                if set(pkg_field) != set(lock_field):
                    error(f"package-lock.json root {field} does not equal package.json {field}")

# -- 2. release/VERSION gating -----------------------------------------------

version_text = read_utf8(root / "release/VERSION", "release/VERSION")
if version_text is not None:
    release_version = version_text.strip()
    if re.fullmatch(r"\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?", release_version) is None:
        error(f"release/VERSION is not Semantic Versioning 2.0.0: {release_version!r}")
    else:
        probe = subprocess.run(
            ["node", "-e",
             "import('./developer-docs/docs.config.mjs').then(m => "
             "console.log(JSON.stringify(m.docsConfig)))"],
            cwd=root, capture_output=True, text=True)
        if probe.returncode != 0:
            error(f"docs.config.mjs could not be evaluated with Node: {probe.stderr.strip()}")
        else:
            try:
                config = json.loads(probe.stdout.strip().splitlines()[-1])
                if config.get("releaseVersion") != release_version:
                    error(f"docs.config.mjs releaseVersion {config.get('releaseVersion')!r} "
                          f"!= release/VERSION {release_version!r}")
                series = ".".join(release_version.split(".")[:2])
                if config.get("docsSeries") != series:
                    error(f"docs.config.mjs docsSeries {config.get('docsSeries')!r} != {series!r}")
            except json.JSONDecodeError as exc:
                error(f"docs.config.mjs did not return JSON: {exc}")

# -- 3. astro/starlight config skeleton --------------------------------------

astro_cfg = read_utf8(docs / "astro.config.mjs", "developer-docs/astro.config.mjs")
if astro_cfg is not None:
    for needle, hint in [
        ("base: '/docs'", "docs must be served under /docs/"),
        ("defaultLocale: 'root'", "zh-CN must be the root locale"),
        ("@astrojs/starlight", "starlight integration required"),
        ("customCss", "brand css must be registered"),
    ]:
        if needle not in astro_cfg:
            error(f"astro.config.mjs missing {needle!r} ({hint})")
    for banned in ("docusaurus", "ssr-polyfill", "BannerPlugin", "elk-stub"):
        if banned.lower() in astro_cfg.lower():
            error(f"astro.config.mjs must not reference {banned!r}")

# -- 4. zh route skeleton -----------------------------------------------------

ZH_ROUTES = [
    "index.mdx",
    "getting-started/android.mdx",
    "concepts/sdk-architecture.mdx",
    "reference/overview.mdx",
    "reference/types.mdx",
    "reference/engine.mdx",
    "reference/session.mdx",
    "reference/surface.mdx",
    "reference/input.mdx",
    "reference/external-frames.mdx",
    "reference/capabilities.mdx",
    "release/download-verification.mdx",
]
docs_root = docs / "src/content/docs"
for rel in ZH_ROUTES:
    path = docs_root / rel
    text = read_utf8(path, f"src/content/docs/{rel}")
    if text is None:
        continue
    fm = re.match(r"^---\n([\s\S]*?)\n---", text)
    if fm is None:
        error(f"src/content/docs/{rel} has no YAML frontmatter")
        continue
    frontmatter = fm.group(1)
    for field in ("title:", "description:"):
        if field not in frontmatter:
            error(f"src/content/docs/{rel} frontmatter is missing {field!r}")


def mermaid_blocks(text: str) -> list[str]:
    return re.findall(r"```mermaid\n([\s\S]*?)```", text)


arch = read_utf8(docs_root / "concepts/sdk-architecture.mdx", "sdk-architecture.mdx")
if arch is not None:
    diagrams = mermaid_blocks(arch)
    if len(diagrams) < 2:
        error(f"sdk-architecture.mdx needs at least 2 Mermaid diagrams, got {len(diagrams)}")
    # every diagram must be followed by a prose explanation ('### 文字说明')
    segments = arch.split("```mermaid")
    for index, segment in enumerate(segments[1:], start=1):
        after = segment.split("```", 1)
        if len(after) < 2 or "### 文字说明" not in after[1][:400]:
            error(f"Mermaid diagram {index} is not immediately followed by '### 文字说明'")
        elif re.search(r"\n\s*1\.\s", after[1][:2000]) is None and "|" not in after[1][:2000]:
            error(f"Mermaid diagram {index} text explanation has no ordered list or table")
    for term in ("migo.*", "capability", "V8", "Skia"):
        if term not in arch:
            error(f"sdk-architecture.mdx is missing required boundary term: {term}")

# -- 5. en 页形状对称(占位 stub 或完整翻译均合法;半翻译是非法状态) ---------

for rel in ZH_ROUTES:
    en_rel = "en/" + rel
    path = docs_root / en_rel
    text = read_utf8(path, f"src/content/docs/{en_rel}")
    if text is None:
        continue
    body = text.split("---", 2)[-1]
    is_pending = ":::note[Translation pending]" in text
    has_noindex = "content: noindex" in text
    if is_pending != has_noindex:
        error(f"src/content/docs/{en_rel} noindex/pending-note 不对称")
        continue
    if is_pending:
        if "(link below) is the current source of truth" not in text:
            error(f"src/content/docs/{en_rel} is missing stub marker 'current source of truth'")
        zh_slug = rel[:-4] if rel.endswith(".mdx") else rel
        zh_href = "/docs/" if zh_slug == "index" else f"/docs/{zh_slug}/"
        if f"[→ 中文版本]({zh_href})" not in text:
            error(f"src/content/docs/{en_rel} must link back to {zh_href}")
        if mermaid_blocks(body):
            error(f"src/content/docs/{en_rel} contains translated Mermaid content")

# -- 5b. 英文实译页只链向英文页 --------------------------------------------------
#
# Starlight 不改写正文里的绝对链接。英文页写 `/docs/reference/types/` 就把读者
# 第一次点击送进中文页 —— 2026-09-19 审查时 latest 英文页的 90 处站内链接全是
# 这样。占位页(Translation pending)的回链指向中文是设计,不在此列。

en_link = re.compile(r'(?:href="|\]\()(/docs/[^"\)#\s]*)')
for page in sorted((docs_root / "en").rglob("*.mdx")):
    rel = page.relative_to(docs_root).as_posix()
    if rel.startswith("en/0.9/"):
        continue
    text = page.read_text(encoding="utf-8")
    if ":::note[Translation pending]" in text:
        continue
    for href in en_link.findall(text):
        if not href.startswith("/docs/en/"):
            error(f"src/content/docs/{rel} 链向中文路由 {href}(英文页应链 /docs/en/…)")

# -- 5c. 0.9 archive is the complete 0.9.7 release baseline -------------------

def mdx_routes(base: pathlib.Path, excluded_top_level: set[str] | None = None) -> set[str]:
    excluded = excluded_top_level or set()
    routes: set[str] = set()
    for page in base.rglob("*.mdx"):
        rel = page.relative_to(base)
        if rel.parts and rel.parts[0] in excluded:
            continue
        routes.add(rel.as_posix())
    return routes


current_zh_routes = mdx_routes(docs_root, {"0.9", "en"})
archive_zh_routes = mdx_routes(docs_root / "0.9")
current_en_routes = mdx_routes(docs_root / "en", {"0.9"})
archive_en_routes = mdx_routes(docs_root / "en" / "0.9")

for rel in sorted(current_zh_routes - archive_zh_routes):
    error(f"0.9 归档缺中文页面:{rel}")
for rel in sorted(archive_zh_routes - current_zh_routes):
    error(f"0.9 归档多出过期中文页面:{rel}")
for rel in sorted(current_en_routes - archive_en_routes):
    error(f"en/0.9 归档缺英文页面:{rel}")
for rel in sorted(archive_en_routes - current_en_routes):
    error(f"en/0.9 归档多出过期英文页面:{rel}")

archive_link = re.compile(r'(?:href="|\]\()(/docs/[^"\)#\s]*)')
for locale_dir, required_prefix in (
    (docs_root / "0.9", "/docs/0.9/"),
    (docs_root / "en" / "0.9", "/docs/en/0.9/"),
):
    for page in sorted(locale_dir.rglob("*.mdx")):
        for href in archive_link.findall(page.read_text(encoding="utf-8")):
            if not href.startswith(required_prefix):
                rel = page.relative_to(docs_root).as_posix()
                error(f"{rel} 链出版本归档 {href}(应链 {required_prefix}…)")

for page in sorted((docs_root / "en" / "0.9").rglob("*.mdx")):
    text = page.read_text(encoding="utf-8")
    rel = page.relative_to(docs_root).as_posix()
    if ":::note[Translation pending]" in text or "content: noindex" in text:
        error(f"{rel} 仍是英文占位页,0.9.7 归档必须使用完整翻译")

def api_sections(text: str) -> set[str]:
    # starlight-versions escapes underscores in generated Markdown headings.
    # Normalize only that Markdown escape before comparing symbol names.
    return set(re.findall(r"^## (migo_[a-z0-9_]+)\s*$", text.replace(r"\_", "_"), flags=re.M))


for page in sorted((docs_root / "reference").glob("*.mdx")):
    current_sections = api_sections(page.read_text(encoding="utf-8"))
    for archive_dir in (docs_root / "0.9" / "reference", docs_root / "en" / "0.9" / "reference"):
        archived = archive_dir / page.name
        if not archived.is_file():
            continue
        archived_sections = api_sections(archived.read_text(encoding="utf-8"))
        rel = archived.relative_to(docs_root).as_posix()
        for name in sorted(current_sections - archived_sections):
            error(f"{rel} 缺函数节 {name}")
        for name in sorted(archived_sections - current_sections):
            error(f"{rel} 多出函数节 {name}")

# -- 6. Docusaurus remains -----------------------------------------------------

remnants = {
    docs / "docusaurus.config.ts": True,
    docs / "docusaurus.config.ts.bak": True,
    docs / "sidebars.ts": True,
    docs / "versions.json": True,
    docs / "versioned_docs": True,
    docs / "versioned_sidebars": True,
    docs / "i18n" / "en" / "docusaurus-plugin-content-docs": True,
    docs / "src" / "theme": True,
    docs / "src" / "components" / "TranslationPending": True,
    docs / "scripts" / "ssr-polyfill.cjs": True,
    docs / "scripts" / "build-search.mjs": True,
    root / "scripts" / "check-developer-docs-ast.mjs": True,
}
for path in remnants:
    if path.exists():
        error(f"Docusaurus-era file still present: {path.relative_to(root)}")

# -- 7. C ABI reference invariants(C API spec 不变量)--------------------------
#
# (a) 每个 header 里 MIGO_API 标记的公开函数,在 latest `reference/*.mdx` 里
#     恰好有一个 `## migo_*` 函数节;每个函数节的名字必须能在 header 里找到
#     声明(双向)。
# (b) latest `reference/*.mdx` 每个 ```c 示例块里调用的 migo_* 函数名,都必须
#     能在 tests/c_host/**/*.c 的源码里找到(近似祖先检查)。

ref_dir = docs_root / "reference"
ref_pages = sorted(ref_dir.glob("*.mdx")) if ref_dir.is_dir() else []
sections: dict[str, str] = {}
duplicate_sections: set[str] = set()
for page in ref_pages:
    for heading in re.findall(r"^## (migo_[a-z0-9_]+)\s*$", page.read_text(encoding="utf-8"), flags=re.M):
        if heading in sections:
            duplicate_sections.add(heading)
        sections[heading] = page.name
for name in sorted(duplicate_sections):
    error(f"C ABI 函数节重复:{name} 同时在 {sections[name]} 与另一页")

# 英文实译的参考页与中文逐页同一组函数节:上面的 header 对照只读中文页,
# 新入口只补中文时,英文读者看不到它,且没有任何检查会说。
for page in ref_pages:
    en_page = docs_root / "en" / "reference" / page.name
    if not en_page.is_file():
        continue
    en_text = en_page.read_text(encoding="utf-8")
    if ":::note[Translation pending]" in en_text:
        continue
    zh_set = set(re.findall(r"^## (migo_[a-z0-9_]+)\s*$", page.read_text(encoding="utf-8"), flags=re.M))
    en_set = set(re.findall(r"^## (migo_[a-z0-9_]+)\s*$", en_text, flags=re.M))
    for name in sorted(zh_set - en_set):
        error(f"en/reference/{page.name} 缺函数节 {name}(中文页有)")
    for name in sorted(en_set - zh_set):
        error(f"en/reference/{page.name} 多出函数节 {name}(中文页无)")

headers_dir = root / "include" / "migo"
header_symbols: set[str] = set()
marked_api: set[str] = set()
for header in headers_dir.glob("*.h"):
    text = header.read_text(encoding="utf-8")
    header_symbols.update(re.findall(r"\b(migo_(?:engine|session|surface|query)_[a-z0-9_]+)\s*\(", text))
    # MIGO_API 标记入口
    marked_api.update(re.findall(r"MIGO_API\s+\w+\s+MIGO_CALL\s+(migo_[a-z0-9_]+)\s*\(", text))
    # 未打宏但签名形态相同的公开入口(external_frames、release_destroy、
    # lifecycle/focus/visibility/vsync 一族)同属文档义务面
    marked_api.update(re.findall(r"(?:^|\n)\s*(?:MIGO_API\s+)?MigoResult\s+(?:MIGO_CALL\s+)?\n?\s*(migo_[a-z0-9_]+)\s*\(", text))

for name in sorted(marked_api - set(sections)):
    error(f"header 公开函数缺函数节:{name}(reference/*.mdx 无 `## {name}`)")
# 带 MIGO_API 的声明本身就是调用形态;只认上面四个前缀时,新的一族
# (migo_sync_reply_*)明明在 header 里声明了,也会被报成找不到
for name in sorted(set(sections) - header_symbols - marked_api):
    error(f"函数节无 header 声明对应:{name}(在 include/migo/*.h 找不到调用形态)")

# 示例祖先:所有 ```c 块内的 migo_* 调用都要在 tests/c_host 有真实调用形态
host_sources = ""
chost = root / "tests" / "c_host"
if chost.is_dir():
    for src in chost.rglob("*.c"):
        host_sources += src.read_text(encoding="utf-8", errors="replace") + "\n"
example_fns: set[tuple[str, str]] = set()
for page in ref_pages:
    text = page.read_text(encoding="utf-8")
    for block in re.findall(r"```c\n([\s\S]*?)```", text):
        fns = frozenset(set(re.findall(r"\b(migo_[a-z0-9_]+)\s*\(", block)))
        if "以头文件为准" in block:
            # 衍生示例:tests/c_host 暂无覆盖路径,块内已按头文件签名写出并标记
            continue
        for fn in fns:
            example_fns.add((page.name, fn))
if host_sources:
    for page_name, fn in sorted(example_fns):
        if fn + "(" not in host_sources:
            error(f"示例无 tests/c_host 祖先:{page_name} 调用 {fn}(tests/c_host/**/*.c 未出现;无覆盖路径时块内需标「以头文件为准」)")

# -- verdict -------------------------------------------------------------------

if errors:
    print("developer-docs phase1 contract FAILED:", file=sys.stderr)
    for line in errors:
        print(f"  - {line}", file=sys.stderr)
    sys.exit(1)

print("developer-docs phase1 contract: PASS")
PY
