#!/usr/bin/env node
// Validates that sourcePaths in MDX frontmatter actually exist in the migo repo.
import {readFileSync, existsSync, readdirSync, statSync} from 'node:fs';
import {resolve, dirname} from 'node:path';
import {fileURLToPath} from 'node:url';

const dir = dirname(fileURLToPath(import.meta.url));
const docsDir = resolve(dir, '..');
const repoRoot = resolve(docsDir, '..');

let failures = 0;
function bad(msg) { failures++; console.error(`  ✗ ${msg}`); }
function ok(msg) { console.log(`  ✓ ${msg}`); }

function walkMdx(startDir) {
  const results = [];
  let entries;
  try { entries = readdirSync(startDir); } catch { return results; }
  for (const entry of entries) {
    const full = resolve(startDir, entry);
    let stat;
    try { stat = statSync(full); } catch { continue; }
    if (stat.isDirectory()) {
      results.push(...walkMdx(full));
    } else if (entry.endsWith('.mdx')) {
      results.push(full);
    }
  }
  return results;
}

const mdxFiles = [
  ...walkMdx(resolve(docsDir, 'src', 'content', 'docs')),
];

for (const file of mdxFiles) {
  const content = readFileSync(file, 'utf8');
  const fm = content.match(/^---\n([\s\S]*?)\n---/);
  if (!fm) continue;
  const frontmatter = fm[1];
  // Extract sourcePaths YAML list
  const spMatch = frontmatter.match(/^sourcePaths:\s*\n((?:\s+-[^\n]+\n?)*)/m);
  if (!spMatch) continue;
  const paths = spMatch[1]
    .split('\n')
    .map(l => l.trim().replace(/^-\s*/, ''))
    .filter(Boolean);
  for (const p of paths) {
    const abs = resolve(repoRoot, p);
    if (!existsSync(abs)) {
      bad(`${file}: sourcePath not found: ${p}`);
    }
  }
}

if (failures > 0) {
  console.error(`\n✗ ${failures} sourcePath validation failure(s)`);
  process.exit(1);
}
ok(`All sourcePaths in ${mdxFiles.length} MDX files exist`);
