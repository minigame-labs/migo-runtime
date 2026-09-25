// Emit WebGL query argument records for the Rust decoder to read.
//
// The producer encodes these and `frame_wire::sync::GlQueryParams::decode`
// reads them. Both were written from the same description, which is the right
// way to write them and no evidence at all that they agree: the way a
// disagreement reaches a user is `getUniformLocation` asked about the wrong
// program, or a name read one byte short -- a uniform that silently does
// nothing, in a frame that otherwise draws.
//
// The spread is chosen for the mistakes a hand-written decoder makes: a name
// whose length is not a multiple of four (padding), an empty name (no payload
// at all), multi-byte UTF-8 (a length in bytes, not characters), and every
// kind, because the kind is what the host dispatches on.
//
// Deterministic: same seed, same bytes, every run.
//
// Usage: node emit-gl-query-params.mjs <output-directory> [count]

import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import {
  CANVAS2D_FLAG_BOLD,
  CANVAS2D_FLAG_ITALIC,
  CANVAS2D_QUERY_MEASURE_TEXT,
  CANVAS2D_QUERY_TEXT_LINE_HEIGHT,
  GL_QUERY_ACTIVE_ATTRIB,
  GL_QUERY_ACTIVE_UNIFORM,
  GL_QUERY_ATTRIB_LOCATION,
  GL_QUERY_CHECK_FRAMEBUFFER_STATUS,
  GL_QUERY_CLIENT_WAIT_SYNC,
  GL_QUERY_GET_ERROR,
  GL_QUERY_PARAMETER,
  GL_QUERY_PROGRAM_INFO_LOG,
  GL_QUERY_PROGRAM_PARAMETER,
  GL_QUERY_QUERY_PARAMETER,
  GL_QUERY_SHADER_INFO_LOG,
  GL_QUERY_SHADER_PARAMETER,
  GL_QUERY_TRANSFORM_FEEDBACK_VARYING,
  GL_QUERY_UNIFORM_BLOCK_INDEX,
  GL_QUERY_UNIFORM_LOCATION,
  encodeCanvas2DQueryParams,
  encodeGlQueryParams,
} from "../src/sync-mailbox.mjs";

const outputDirectory = process.argv[2];
if (!outputDirectory) {
  console.error("usage: emit-gl-query-params.mjs <output-directory> [count]");
  process.exit(2);
}
const count = Number(process.argv[3] ?? 60);
mkdirSync(outputDirectory, { recursive: true });

const KINDS = [
  GL_QUERY_PROGRAM_PARAMETER,
  GL_QUERY_SHADER_PARAMETER,
  GL_QUERY_QUERY_PARAMETER,
  GL_QUERY_CHECK_FRAMEBUFFER_STATUS,
  GL_QUERY_CLIENT_WAIT_SYNC,
  GL_QUERY_GET_ERROR,
  GL_QUERY_UNIFORM_LOCATION,
  GL_QUERY_ATTRIB_LOCATION,
  GL_QUERY_UNIFORM_BLOCK_INDEX,
  GL_QUERY_PROGRAM_INFO_LOG,
  GL_QUERY_SHADER_INFO_LOG,
  GL_QUERY_PARAMETER,
  GL_QUERY_ACTIVE_ATTRIB,
  GL_QUERY_ACTIVE_UNIFORM,
  GL_QUERY_TRANSFORM_FEEDBACK_VARYING,
];

// Names of every length modulo four, plus one that is not ASCII: the length is
// a byte count and a decoder that counted characters would read past the end.
const NAMES = [
  "",
  "u",
  "uv",
  "pos",
  "uColor",
  "aVertexPosition",
  "uMatrices[3].offset",
  "uéè",
  "纹理",
];

let seed = 0x1234_5678;
function next(bound) {
  seed ^= seed << 13;
  seed ^= seed >>> 17;
  seed ^= seed << 5;
  seed >>>= 0;
  return seed % bound;
}

/** JSON's escaping for the manifest, which the Rust reader searches as text. */
function escape(text) {
  return JSON.stringify(text).slice(1, -1);
}

const encoder = new TextEncoder();
const manifest = [];
for (let index = 0; index < count; index += 1) {
  const kind = KINDS[index % KINDS.length];
  const name = NAMES[next(NAMES.length)];
  const record = {
    kind,
    canvasId: 1 + next(8),
    object: next(4096),
    pname: next(0x9000),
    extra: next(1024),
    name,
  };
  const bytes = encodeGlQueryParams(record);
  const file = `query-${String(index).padStart(4, "0")}.bin`;
  writeFileSync(join(outputDirectory, file), bytes);
  manifest.push(
    `{"file":"${file}","kind":${record.kind},"canvas_id":${record.canvasId},` +
      `"object":${record.object},"pname":${record.pname},"extra":${record.extra},` +
      `"name":"${escape(name)}","name_bytes":${encoder.encode(name).byteLength},` +
      `"total_bytes":${bytes.byteLength}}`,
  );
}

writeFileSync(join(outputDirectory, "manifest.jsonl"), `${manifest.join("\n")}\n`);

// The Canvas2D queries, whose arguments are two strings rather than one: a text
// and a font, a family and a size. The pair is where an encoder goes wrong --
// the second length read from the first's padded end, or a size that crossed as
// a double -- so both strings vary in length modulo four and one is not ASCII.
const TEXTS = ["", "A", "hi", "score: 0", "\u4e2d\u6587\u6807\u9898", "The quick brown fox"];
const FONTS = ["16px sans-serif", "italic bold 24px 'Noto Sans'", "sans-serif", ""];
const canvas2d = [];
for (let index = 0; index < TEXTS.length * FONTS.length; index += 1) {
  const text = TEXTS[index % TEXTS.length];
  const font = FONTS[index % FONTS.length];
  const measure = index % 2 === 0;
  const record = {
    kind: measure ? CANVAS2D_QUERY_MEASURE_TEXT : CANVAS2D_QUERY_TEXT_LINE_HEIGHT,
    canvasId: 1 + (index % 4),
    // A size with a fractional part, so a `f32` that crossed as something else
    // is a different number rather than the same integer.
    number: measure ? 0 : 12.5 + index,
    flags: (index % 2 === 0 ? CANVAS2D_FLAG_BOLD : 0) | (index % 3 === 0 ? CANVAS2D_FLAG_ITALIC : 0),
    text,
    font,
  };
  const bytes = encodeCanvas2DQueryParams(record);
  const file = `canvas2d-${String(index).padStart(4, "0")}.bin`;
  writeFileSync(join(outputDirectory, file), bytes);
  canvas2d.push(
    `{"file":"${file}","kind":${record.kind},"canvas_id":${record.canvasId},` +
      `"number":${Math.fround(record.number)},"flags":${record.flags},` +
      `"text":"${escape(text)}","text_bytes":${encoder.encode(text).byteLength},` +
      `"font":"${escape(font)}","font_bytes":${encoder.encode(font).byteLength},` +
      `"total_bytes":${bytes.byteLength}}`,
  );
}
writeFileSync(join(outputDirectory, "canvas2d-manifest.jsonl"), `${canvas2d.join("\n")}\n`);

console.log(
  `emitted ${manifest.length} query records and ${canvas2d.length} Canvas2D query records into ${outputDirectory}`,
);
