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
console.log(`emitted ${manifest.length} query records into ${outputDirectory}`);
