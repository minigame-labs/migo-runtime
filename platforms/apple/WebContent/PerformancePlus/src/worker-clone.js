// Classic-script backend, also imported by worker-clone.mjs. Capture native
// primitives before content runs. Never interpret V8's private wire format.
(() => {
  // iOS 15.0-15.3 predates structuredClone, but PerformanceMark.detail uses
  // WebKit's native serializer since Safari 14.1. Constructing a mark directly
  // does not add a timeline entry. Wrap undefined (detail alone maps it to null).
  const NativeMark = globalThis.PerformanceMark;
  const markDetail = NativeMark && Object.getOwnPropertyDescriptor(NativeMark.prototype, "detail")?.get;
  const readMark = markDetail && Function.prototype.call.bind(markDetail);
  const clone = typeof globalThis.structuredClone === "function"
    ? globalThis.structuredClone.bind(globalThis)
    : (value) => readMark(new NativeMark("migo.worker.clone", { startTime: 0, detail: { value } })).value;
  const call = (fn) => Function.prototype.call.bind(fn);
  const ownKeys = Object.getOwnPropertyNames;
  const descriptor = Object.getOwnPropertyDescriptor;
  const prototype = Object.getPrototypeOf;
  const objectPrototype = Object.prototype;
  const arrayPrototype = Array.prototype;
  const hasInstance = call(Function.prototype[Symbol.hasInstance]);
  const isView = ArrayBuffer.isView;
  const arrayBufferBytes = call(descriptor(ArrayBuffer.prototype, "byteLength").get);
  const sharedBufferBytes = typeof SharedArrayBuffer === "function"
    ? call(descriptor(SharedArrayBuffer.prototype, "byteLength").get) : null;
  const typedBuffer = call(descriptor(prototype(Uint8Array.prototype), "buffer").get);
  const dataViewBuffer = call(descriptor(DataView.prototype, "buffer").get);
  const regexpSource = call(descriptor(RegExp.prototype, "source").get);
  const mapEntries = call(Map.prototype.entries);
  const setValues = call(Set.prototype.values);
  const mapNext = call(prototype(new Map().entries()).next);
  const setNext = call(prototype(new Set().values()).next);
  const mapGet = call(Map.prototype.get), mapSet = call(Map.prototype.set);
  const mapHas = call(Map.prototype.has), mapDelete = call(Map.prototype.delete);
  const setHas = call(Set.prototype.has), setAdd = call(Set.prototype.add);
  const arrayPush = call(Array.prototype.push);
  const bigintText = call(BigInt.prototype.toString);
  const boxedString = call(String.prototype.valueOf), boxedBigInt = call(BigInt.prototype.valueOf);
  const bigintPrototype = BigInt.prototype;
  const apply = Reflect.apply;
  const ErrorClass = Error, RangeErrorClass = RangeError, MapClass = Map, SetClass = Set;
  const ArrayBufferClass = ArrayBuffer, DataViewClass = DataView;
  const DateClass = Date, RegExpClass = RegExp;
  const NumberClass = Number, BooleanClass = Boolean, StringClass = String;
  const numberInteger = Number.isInteger;
  const minimum = Math.min;

  function fail(message, name = "DataCloneError") {
    const error = new ErrorClass(message);
    error.name = name;
    throw error;
  }

  // Account the already cloned graph, using only captured intrinsics. Native
  // clone flattens ordinary getters once; this walk never reads them again.
  // This bounds retained logical payload, not WebKit's native clone allocation.
  function describe(value, maxBytes) {
    const seen = new SetClass(), work = [value], transfers = [];
    let bytes = 0;
    const charge = (amount) => {
      bytes += amount;
      if (bytes > maxBytes) throw new RangeErrorClass("Worker message too large");
    };
    for (let index = 0; index < work.length; index++) {
      const item = work[index];
      const kind = typeof item;
      if (kind === "string") { charge(16 + item.length * 2); continue; }
      if (kind === "bigint") { charge(16 + bigintText(item, 16).length); continue; }
      if (item === null || kind !== "object") { charge(16); continue; }
      if (setHas(seen, item)) { charge(8); continue; }
      setAdd(seen, item);
      charge(64);
      if (hasInstance(ArrayBufferClass, item)) {
        charge(arrayBufferBytes(item));
        arrayPush(transfers, item);
      } else if (isView(item)) {
        arrayPush(work, hasInstance(DataViewClass, item) ? dataViewBuffer(item) : typedBuffer(item));
      } else if (sharedBufferBytes !== null && (() => {
        try { sharedBufferBytes(item); return true; } catch (_) { return false; }
      })()) {
        fail("SharedArrayBuffer is not enabled for Worker messages");
      } else if (hasInstance(MapClass, item)) {
        const iterator = mapEntries(item);
        for (let next = mapNext(iterator); !next.done; next = mapNext(iterator)) {
          charge(16); arrayPush(work, next.value[0], next.value[1]);
        }
      } else if (hasInstance(SetClass, item)) {
        const iterator = setValues(item);
        for (let next = setNext(iterator); !next.done; next = setNext(iterator)) {
          charge(8); arrayPush(work, next.value);
        }
      } else if (hasInstance(DateClass, item)) {
        charge(8);
      } else if (hasInstance(RegExpClass, item)) {
        charge(regexpSource(item).length * 2 + 16);
      } else if (hasInstance(StringClass, item)) {
        charge(boxedString(item).length * 2 + 16);
      } else if (hasInstance(NumberClass, item) || hasInstance(BooleanClass, item)) {
        charge(8);
      } else if (prototype(item) === bigintPrototype) {
        charge(bigintText(boxedBigInt(item), 16).length + 16);
      } else {
        const proto = prototype(item);
        if (proto !== objectPrototype && proto !== arrayPrototype && proto !== null
            && !hasInstance(ErrorClass, item)) {
          fail("This value type is not enabled for Worker messages");
        }
        const keys = ownKeys(item);
        for (let keyIndex = 0; keyIndex < keys.length; keyIndex++) {
          const key = keys[keyIndex];
          const property = descriptor(item, key);
          charge(key.length * 2 + 16);
          if ("value" in property) arrayPush(work, property.value);
          else if (key === "stack" && hasInstance(ErrorClass, item) && property.get) {
            // V8 keeps the already cloned stack string behind a native own
            // accessor. It is not the original content object's getter.
            arrayPush(work, apply(property.get, item, []));
          } else fail("Unexpected accessor in a cloned Worker value");
        }
      }
    }
    return { value, bytes, transfers };
  }

  function encodeMessage(value, maxBytes) {
    if (!numberInteger(maxBytes) || maxBytes < 1 || maxBytes > 16 * 1024 * 1024) {
      throw new RangeErrorClass("Invalid Worker message size limit");
    }
    return describe(clone(value), maxBytes);
  }
  function decodeMessage(wire) { return wire.value; }
  function wireBytes(wire) { return wire.bytes; }
  function receivedWire(value, bytes) { return { value, bytes, transfers: [] }; }

  function createBudget() {
    const entries = new MapClass();
    let next = 0, used = 0, count = 0, closed = false;
    function abort(ticket) {
      const entry = mapGet(entries, ticket);
      if (entry === undefined) return;
      used -= entry.bytes; count--; mapDelete(entries, ticket);
    }
    return {
      reserve(maxBytes) {
        if (closed) fail("Worker has been terminated", "WorkerError");
        if (!numberInteger(maxBytes) || maxBytes < 1 || maxBytes > 16 * 1024 * 1024) {
          fail("Invalid Worker message size limit", "WorkerError");
        }
        if (count >= 4096) fail("Worker message queue full", "WorkerError");
        maxBytes = minimum(maxBytes, 64 * 1024 * 1024 - used);
        if (maxBytes <= 0) fail("Worker message queue byte limit exceeded", "WorkerError");
        do { next = (next % 0x7fffffff) + 1; } while (mapHas(entries, next));
        mapSet(entries, next, { bytes: maxBytes, committed: false });
        used += maxBytes; count++;
        return { ticket: next, maxBytes };
      },
      commit(ticket, wire) {
        const entry = mapGet(entries, ticket);
        if (closed || entry === undefined || entry.committed) fail("Worker send reservation expired", "WorkerError");
        if (!numberInteger(wire.bytes) || wire.bytes < 1 || wire.bytes > entry.bytes) {
          abort(ticket); fail("Worker message queue byte limit exceeded", "WorkerError");
        }
        used -= entry.bytes - wire.bytes;
        entry.bytes = wire.bytes; entry.committed = true;
      },
      // finally abort only cancels unfinished serialization. A committed send
      // retains its credit until the destination dequeues it.
      abort(ticket) { if (mapGet(entries, ticket)?.committed !== true) abort(ticket); },
      acknowledge(ticket) { if (mapGet(entries, ticket)?.committed === true) abort(ticket); },
      close() {
        closed = true;
        const iterator = mapEntries(entries);
        for (let step = mapNext(iterator); !step.done; step = mapNext(iterator)) abort(step.value[0]);
      },
      // Used by deterministic tests and diagnostics; never installed globally.
      usage() { return { count, bytes: used, closed }; },
    };
  }

  Object.defineProperty(globalThis, "__migoWorkerCloneBackend", {
    configurable: true,
    value: { encodeMessage, decodeMessage, wireBytes, receivedWire, createBudget },
  });
})();
