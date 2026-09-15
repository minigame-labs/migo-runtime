// EN stub 对齐器(IA spec §6.1):
//   每个 zh 页必须有 en/<相同路径> 的 stub。
//   有英文实译的页(含 :::note[Translation pending] 之外的实体正文)不在本脚本干预范围。
// 两种模式:
//   node scripts/ensure-en-stubs.mjs           生成缺失的 stub(不覆盖已存在文件)
//   node scripts/ensure-en-stubs.mjs --assert  只校对不齐就非零退出(构建门禁)
// Stub 模板 = zh frontmatter 的机器可用的骨架(title/description 原样 + noindex + 回链);
// 手翻替换时删除 noindex 与 note 即成真译,本脚本不会再碰它。
import { readdirSync, readFileSync, existsSync, mkdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const assertOnly = process.argv.includes('--assert');
const docsDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../src/content/docs');

function* walk(dir) {
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, e.name);
    if (e.isDirectory()) yield* walk(full);
    else if (e.name.endsWith('.mdx')) yield full;
  }
}

function parseFrontmatter(text) {
  const m = text.match(/^---\n([\s\S]*?)\n---\n/);
  if (!m) failPage('missing frontmatter');
  const title = m[1].match(/^title: (.*)$/m)?.[1];
  const description = m[1].match(/^description: (.*)$/m)?.[1];
  if (!title || !description) failPage('frontmatter needs title/description');
  return { title, description };
}

function failPage(msg, file) {
  console.error(`ensure-en-stubs: ${msg}${file ? ` — ${file}` : ''}`);
  process.exit(1);
}

function zhSlugOf(zhFile) {
  return path.relative(docsDir, zhFile);
}

let touched = 0;
for (const zhFile of walk(docsDir)) {
  const rel = zhSlugOf(zhFile);
  if (rel.startsWith(`en${path.sep}`)) continue;
  const enFile = path.join(docsDir, 'en', rel);
  if (existsSync(enFile)) {
    // 对现有 stub 收敛两个不变量:有 pending note ⇒ 必须 noindex;反之亦然。
    const body = readFileSync(enFile, 'utf8');
    const isPending = body.includes(':::note[Translation pending]');
    const hasNoindex = /name: robots\s*\n\s*content: noindex/.test(body);
    if (isPending !== hasNoindex) {
      failPage(`noindex/pending-note 不对称(isPending=${isPending}, hasNoindex=${hasNoindex})`, enFile);
    }
    continue;
  }
  if (assertOnly) failPage('zh 页缺 en stub(运行 node scripts/ensure-en-stubs.mjs 补齐)', enFile);
  const { title, description } = parseFrontmatter(readFileSync(zhFile, 'utf8'));
  // rel 形如 'getting-started/android.mdx' → '/getting-started/android/';顶层 index → '/'
  const slug = `/${rel.slice(0, -'.mdx'.length).replaceAll('\\', '/')}/`
    .replace(/\/index\/$/, '/')
    .replace(/\/+/g, '/');
  const stub = `---
title: ${title}
description: ${description}
head:
  - tag: meta
    attrs:
      name: robots
      content: noindex
---

:::note[Translation pending]
This page is pending translation. The 中文版本 (link below) is the current source of truth.
:::

[→ 中文版本](/docs${slug})
`;
  mkdirSync(path.dirname(enFile), { recursive: true });
  writeFileSync(enFile, stub, 'utf8');
  touched += 1;
}
console.log(`ensure-en-stubs: ${assertOnly ? 'parity ok' : `generated ${touched} stub(s)`}`);
