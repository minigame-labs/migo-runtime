// Execute the production classic bootstrap in a real Node worker thread. This
// supplies only Web Worker host primitives; it does not emulate clone or ports.
import { parentPort, workerData } from "node:worker_threads";
import { readFileSync } from "node:fs";
import vm from "node:vm";

globalThis.self = globalThis;
if (workerData.legacyClone) globalThis.structuredClone = undefined;
globalThis.location = new URL(workerData.bootstrap);
globalThis.importScripts = (...paths) => {
  for (const path of paths) vm.runInThisContext(readFileSync(new URL(path), "utf8"), { filename: path });
};
parentPort.on("message", (data) => globalThis.onmessage?.({ data }));
vm.runInThisContext(readFileSync(new URL(workerData.bootstrap), "utf8"), { filename: workerData.bootstrap });
