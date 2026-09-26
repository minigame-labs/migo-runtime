// Shared docs metadata, imported by astro.config.mjs and by
// ../scripts/test-developer-docs-phase1-contract.sh for contract checks.
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const siteDir = dirname(fileURLToPath(import.meta.url));
const releaseVersion = readFileSync(resolve(siteDir, '../release/VERSION'), 'utf8').trim();

if (!/^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/.test(releaseVersion)) {
  throw new Error(`invalid release/VERSION: ${releaseVersion}`);
}

export const docsConfig = {
  /** e.g. "0.9.7" — full SDK version from release/VERSION. */
  releaseVersion,
  /** e.g. "0.9" — the docs series; latest stable is served at the /docs/ root. */
  docsSeries: releaseVersion.split('.').slice(0, 2).join('.'),
};
