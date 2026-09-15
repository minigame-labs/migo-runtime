#!/usr/bin/env node
// Validates the Starlight build output in build/ before it is copied into
// migo-www/dist/docs. These are publication gates: crawlable SSG HTML, a real
// Pagefind index for the zh corpus, noindexed en stubs, and no Next/docusaurus
// leftovers.
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const buildDir = resolve(dirname(fileURLToPath(import.meta.url)), '..', 'build');

let failures = 0;
const ok = (m) => console.log(`  ✓ ${m}`);
const bad = (m) => { failures++; console.error(`  ✗ ${m}`); };
const check = (cond, msg) => (cond ? ok(msg) : bad(msg));

const fileExists = (rel) => existsSync(resolve(buildDir, rel));
const readFile = (rel) => readFileSync(resolve(buildDir, rel), 'utf8');

console.log('-- 必出文件 --');
const required = [
  'index.html',
  '404.html',
  'getting-started/android/index.html',
  'concepts/sdk-architecture/index.html',
  'reference/overview/index.html',
  'reference/types/index.html',
  'reference/engine/index.html',
  'reference/session/index.html',
  'reference/surface/index.html',
  'reference/input/index.html',
  'reference/external-frames/index.html',
  'reference/capabilities/index.html',
  'release/download-verification/index.html',
  'en/index.html',
  'en/reference/overview/index.html',
  'pagefind/pagefind.js',
  'sitemap-index.xml',
  'img/favicon.svg',
  'brand/mark.svg',
  'fonts/SmileySans-Oblique.otf.woff2',
];
for (const rel of required) check(fileExists(rel), rel);

console.log('-- 禁出文件 --');
const forbidden = ['next/index.html', 'en/next/index.html', 'next/pagefind/pagefind.js'];
for (const rel of forbidden) check(!fileExists(rel), `${rel} absent (no Next version)`);
for (const rel of ['index.html', 'reference/overview/index.html']) {
  if (fileExists(rel)) {
    const html = readFile(rel);
    check(!html.includes('theme-doc-markdown'), `${rel}: no Docusaurus classnames`);
    check(!/docusaurus/i.test(html.replace(/docusaurus\.(io|dev)/gi, '')), `${rel}: no Docusaurus references`);
  }
}

console.log('-- 中文页面可抓 --');
for (const rel of ['index.html', 'concepts/sdk-architecture/index.html', 'reference/overview/index.html']) {
  if (!fileExists(rel)) continue;
  const html = readFile(rel);
  const canonical = html.match(/<link rel="canonical" href="([^"]+)"/);
  check(!!canonical, `${rel}: has canonical`);
  if (canonical) {
    check(
      canonical[1].startsWith('https://minigame-labs.com/docs/'),
      `${rel}: canonical under /docs/ (got ${canonical[1]})`,
    );
    check(!canonical[1].includes('/en/'), `${rel}: canonical is the zh page`);
  }
  check(!/name="robots"[^>]*noindex/i.test(html), `${rel}: indexable (no noindex)`);
}

// Arch diagram page must contain prose explanations (client-side mermaid means
// the SVG itself is JS-rendered; the prose is the crawlable part).
if (fileExists('concepts/sdk-architecture/index.html')) {
  const html = readFile('concepts/sdk-architecture/index.html');
  check(html.includes('文字说明'), 'sdk-architecture: prose explanation present');
  check((html.match(/pre[^>]*class="[^"]*mermaid/g) || []).length >= 2 || html.toLowerCase().includes('mermaid'),
    'sdk-architecture: mermaid blocks present');
}

console.log('-- 英文占位页 --');
for (const rel of ['en/index.html', 'en/reference/overview/index.html']) {
  if (!fileExists(rel)) continue;
  const html = readFile(rel);
  check(/name="robots"[^>]*content="noindex"/i.test(html), `${rel}: meta robots noindex`);
  check(html.includes('Translation pending'), `${rel}: TranslationPending aside visible`);
  check(html.includes('→ 中文版本'), `${rel}: links back to the zh page`);
}

console.log('-- Sitemap --');
if (!fileExists('sitemap-index.xml')) {
  bad('sitemap-index.xml missing');
} else {
  const index = readFile('sitemap-index.xml');
  const docsMap = index.match(/https:\/\/minigame-labs\.com\/docs\/sitemap-\d+\.xml/);
  check(!!docsMap, 'sitemap-index references a /docs/ sitemap');
  for (const rel of readdirSync(buildDir).filter((f) => /^sitemap-\d+\.xml$/.test(f))) {
    const xml = readFile(rel);
    check(!xml.includes('/docs/en/'), `${rel}: no English stub URLs`);
    check(!xml.includes('/docs/next/'), `${rel}: no next URLs`);
    check(xml.includes('https://minigame-labs.com/docs/getting-started/android/') ||
          !xml.includes('getting-started'), `${rel}: zh routes URL-shaped correctly`);
  }
}

console.log('-- 搜索索引 --');
if (!fileExists('pagefind/pagefind.js')) {
  bad('pagefind/pagefind.js missing');
} else {
  const countFragments = (dir) => readdirSync(dir, { withFileTypes: true }).reduce((total, entry) => {
    const full = resolve(dir, entry.name);
    if (entry.isDirectory()) return total + countFragments(full);
    return total + (entry.name.endsWith('.pf_fragment') ? 1 : 0);
  }, 0);
  const fragments = countFragments(resolve(buildDir, 'pagefind'));
  check(fragments > 5, `pagefind has ${fragments} content fragments (>5)`);
}

if (failures > 0) {
  console.error(`\n✗ ${failures} built-site check failure(s)`);
  process.exit(1);
}
console.log('\n✓ built-site contract: PASS');
