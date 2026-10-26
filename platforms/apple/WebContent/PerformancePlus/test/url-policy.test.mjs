import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import vm from "node:vm";

const staged = resolve(process.argv[2]);
const fixtures = resolve(process.argv[3]);
if (process.env.MIGO_TEST_LEGACY_URL_USV === "1") String.prototype.toWellFormed = undefined;
const facade = await import(pathToFileURL(resolve(staged, "engine/host_v8_url/03_url.js")));

test("shared URL query policy passes applicable unchanged WPT on native browser backend", () => {
  globalThis.URL = facade.URL;
  globalThis.URLSearchParams = facade.URLSearchParams;
  vm.runInThisContext(readFileSync(resolve(fixtures, "harness.js"), "utf8"));
  const files = readdirSync(fixtures).filter(name => name.startsWith("urlsearchparams-")).sort();
  assert.equal(files.length, 10);
  for (const name of files) vm.runInThisContext(`(() => {\n${readFileSync(resolve(fixtures, name), "utf8")}\n})()`);
  assert.equal(__wpt.excluded.length, 2);
  assert.equal(__wpt.passed, 84);
  assert.deepEqual(__wpt.failures, []);
});

test("native backend keeps query identity, reentry and USVString conversion", () => {
  const url = new facade.URL("https://example.test/?q=1&q=2");
  const params = url.searchParams;
  url.search = "?next=3";
  assert.equal(params.get("next"), "3");
  params.append("\uD800", "\uDC00");
  assert.equal(params.get("\uFFFD"), "\uFFFD");
  url.port = { toString() { url.hostname = "reentered.test"; return "8443"; } };
  assert.equal(url.host, "reentered.test:8443");
  assert.equal(new facade.URLSearchParams(null).size, 0);
  params.toString = () => "forged=1";
  params.append("owned", "value");
  assert.match(url.search, /owned=value/);
  assert.doesNotMatch(url.search, /forged/);
  let calls = 0;
  new facade.URLSearchParams({ get [Symbol.iterator]() { calls++; return function* () { yield ["x", "y"]; }; } });
  assert.equal(calls, 1);
  assert.throws(() => new facade.URLSearchParams([{ 0: "x", 1: "y", length: 2 }]), TypeError);
  assert.throws(() => new facade.URLSearchParams({ [Symbol()]: "x" }), TypeError);
});
