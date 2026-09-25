// Put the corpus through the producer's font parser and print one line each.
//
// The Rust half is `engine/crates/shared/tests/css_font_corpus.rs`, and
// `scripts/test-css-font-agreement.sh` diffs the two outputs. The format is
// deliberately flat text: a diff of two files is the whole check, and a reader
// looking at a failure sees the shorthand and both answers side by side.
//
// Usage: node emit-css-font.mjs <corpus file>

import { readFileSync } from "node:fs";

import { parseFontShorthand } from "../src/css-font.mjs";

const corpus = process.argv[2];
if (!corpus) {
  console.error("usage: emit-css-font.mjs <corpus file>");
  process.exit(2);
}

for (const line of readFileSync(corpus, "utf8").split("\n")) {
  if (line.startsWith("#") || line.trim().length === 0) continue;
  const parsed = parseFontShorthand(line);
  if (parsed === null) {
    console.log(`${JSON.stringify(line)}\trefused`);
    continue;
  }
  // Size to three decimals: both sides compute in f32-shaped arithmetic and a
  // full-precision print would fail on the last bit of `12pt` rather than on a
  // disagreement anyone can see.
  console.log(
    `${JSON.stringify(line)}\t${parsed.sizePx.toFixed(3)}\t${parsed.weight}\t` +
      `${parsed.italic}\t${parsed.families.join("|")}`,
  );
}
