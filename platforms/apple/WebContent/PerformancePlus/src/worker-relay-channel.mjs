// Producer side of a direct private port to the nested Worker. The page only
// creates/terminates it; payloads never take an extra page relay/clone hop.
import { engineHost } from "./engine-host.mjs";
import { createBudget, receivedWire } from "./worker-clone.mjs";

const PromiseClass = Promise;
const resolve = Promise.resolve.bind(Promise);
const reject = Promise.reject.bind(Promise);
const uncurry = (fn) => Function.prototype.call.bind(fn);
const push = uncurry(Array.prototype.push), shift = uncurry(Array.prototype.shift);
const slice = uncurry(Array.prototype.slice);
const mapGet = uncurry(Map.prototype.get), mapSet = uncurry(Map.prototype.set);
const mapDelete = uncurry(Map.prototype.delete), mapHas = uncurry(Map.prototype.has);
const mapEntries = uncurry(Map.prototype.entries);
const mapNext = uncurry(Object.getPrototypeOf(new Map().entries()).next);
const ErrorClass = Error, MapClass = Map;
const portPost = uncurry(MessagePort.prototype.postMessage);
const portStart = uncurry(MessagePort.prototype.start), portClose = uncurry(MessagePort.prototype.close);

function failure(message) { const error = new ErrorClass(message); error.name = "WorkerError"; return error; }
function workerRelayPort() {
  const port = engineHost().workerRelayPort;
  if (port === undefined) throw failure("this host did not offer a worker relay");
  return port;
}

class WorkerAsyncQueue {
  #items = []; #head = 0; #waiters = []; #closed = false; #bytes = 0;
  #maxItems; #maxBytes; #take;
  constructor(maxItems, maxBytes, take) { this.#maxItems = maxItems; this.#maxBytes = maxBytes; this.#take = take; }
  push(item, bytes) {
    if (this.#closed) return false;
    if (this.#waiters.length) { shift(this.#waiters)(this.#take(item)); return true; }
    if (this.#items.length - this.#head >= this.#maxItems || bytes > this.#maxBytes - this.#bytes) return false;
    push(this.#items, { item, bytes }); this.#bytes += bytes; return true;
  }
  close() {
    this.#closed = true; this.#items = []; this.#head = 0; this.#bytes = 0;
    while (this.#waiters.length) shift(this.#waiters)(undefined);
  }
  next() {
    if (this.#head < this.#items.length) {
      const entry = this.#items[this.#head]; this.#items[this.#head++] = undefined; this.#bytes -= entry.bytes;
      if (this.#head === this.#items.length) { this.#head = 0; this.#items = []; }
      else if (this.#head > 1024 && this.#head * 2 >= this.#items.length) { this.#items = slice(this.#items, this.#head); this.#head = 0; }
      return resolve(this.#take(entry.item));
    }
    if (this.#closed) return resolve(undefined);
    return new PromiseClass((settle) => push(this.#waiters, settle));
  }
}

let nextAttempt = 1, nextTicket = 0;
let current = null;
const tickets = new MapClass();

function releaseTicket(ticket, committedOnly) {
  const record = mapGet(tickets, ticket);
  if (record === undefined || record.committed !== committedOnly) return;
  if (record.committed) record.attempt.budget.acknowledge(record.local);
  else record.attempt.budget.abort(record.local);
  mapDelete(tickets, ticket);
}

function onMessage(event) {
  const message = event.data, attempt = current;
  if (!message || typeof message !== "object") return;
  if (attempt === null || message.id !== attempt.id) { if (message.port) portClose(message.port); return; }
  switch (message.k) {
    case "created":
      attempt.port = message.port;
      attempt.port.onmessage = (event) => onDataMessage(attempt, event);
      portStart(attempt.port);
      attempt.settled?.resolve(); attempt.settled = null;
      break;
    case "create-failed": attempt.settled?.reject(failure(message.detail || "worker construction failed")); attempt.settled = null; break;
    case "err":
      attempt.errors.push(message, message.detail.length * 2);
      break;
  }
}

function onDataMessage(attempt, event) {
  if (current !== attempt) return;
  const message = event.data;
  if (message.k === "ack") releaseTicket(message.seq, true);
  if (message.k === "msg" && !attempt.messages.push(message, message.bytes)) terminateWorkerAttempt();
}

export function beginWorkerAttempt(path) {
  if (current !== null) throw failure("Only one worker can exist at a time. Call terminate() first.");
  const port = workerRelayPort(); port.onmessage = onMessage;
  const id = nextAttempt++;
  return new PromiseClass((resolve, reject) => {
    const attempt = { id, budget: createBudget(), settled: { resolve, reject } };
    attempt.messages = new WorkerAsyncQueue(4096, 64 * 1024 * 1024, (message) => {
      if (current === attempt) portPost(attempt.port, { k: "ack", seq: message.seq });
      return receivedWire(message.data, message.bytes);
    });
    attempt.errors = new WorkerAsyncQueue(16, 1024 * 1024, (message) => {
      if (current === attempt) portPost(port, { k: "error-ack", id, seq: message.seq });
      return message.detail;
    });
    current = attempt;
    try { portPost(port, { k: "create", id, path }); }
    catch (error) { current = null; attempt.budget.close(); reject(error); }
  });
}

export function reserveWorkerMessage(maxBytes) {
  const attempt = current;
  if (attempt === null) throw failure("No active worker");
  const reservation = attempt.budget.reserve(maxBytes);
  do { nextTicket = (nextTicket % 0x7fffffff) + 1; } while (mapHas(tickets, nextTicket));
  mapSet(tickets, nextTicket, { attempt, local: reservation.ticket, committed: false });
  return { ticket: nextTicket, maxBytes: reservation.maxBytes };
}

export function commitWorkerMessage(ticket, wire) {
  const record = mapGet(tickets, ticket);
  if (record === undefined || record.attempt !== current) throw failure("Worker send reservation expired");
  record.attempt.budget.commit(record.local, wire);
  record.committed = true;
  try {
    portPost(record.attempt.port, { k: "post", seq: ticket, data: wire.value, bytes: wire.bytes }, wire.transfers);
  } catch (error) { releaseTicket(ticket, true); throw error; }
}
export function abortWorkerMessage(ticket) { releaseTicket(ticket, false); }
export function nextWorkerMessage() { return current === null ? reject(failure("No active worker")) : current.messages.next(); }
export function nextWorkerError() { return current === null ? reject(failure("No active worker")) : current.errors.next(); }

export function terminateWorkerAttempt() {
  const attempt = current;
  if (attempt === null) return;
  current = null;
  attempt.settled?.reject(failure("terminated before it was ready")); attempt.settled = null;
  attempt.messages.close(); attempt.errors.close(); attempt.budget.close();
  if (attempt.port) portClose(attempt.port);
  const iterator = mapEntries(tickets);
  for (let step = mapNext(iterator); !step.done; step = mapNext(iterator)) {
    if (step.value[1].attempt === attempt) mapDelete(tickets, step.value[0]);
  }
  portPost(workerRelayPort(), { k: "term", id: attempt.id });
}
