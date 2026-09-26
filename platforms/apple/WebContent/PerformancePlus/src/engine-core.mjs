// What the engine's JavaScript imports from "ext:core/mod.js", for a runtime
// that is not deno_core.
//
// On Performance+ the engine's API layer runs in the producer Worker inside
// WebKit's WebContent process. Its modules import `primordials` and `core` from
// deno_core, and the generated `engine/core/mod.mjs` gets them from here instead.
// The surface is small on purpose and derived rather than guessed:
// `scripts/gen-performance-plus-engine.py` collects every primordials name the
// engine destructures and resolves it at build time through `resolvePrimordial`
// below, so a name this file cannot produce fails the build rather than
// arriving as `undefined` in the middle of a frame.

import { platform } from "./platform.mjs";

const { apply, bind, call } = Function.prototype;
// deno's own construction: `uncurryThis(fn)(self, ...args)` is `fn.call(self,
// ...args)`, bound once so a later change to Function.prototype.call cannot
// reach it.
const uncurryThis = bind.bind(call);
const ReflectApply = Reflect.apply;
const ObjectGetOwnPropertyDescriptor = Object.getOwnPropertyDescriptor;
const ObjectGetOwnPropertyNames = Object.getOwnPropertyNames;
const ObjectFreeze = Object.freeze;
const ObjectGetPrototypeOf = Object.getPrototypeOf;

// The intrinsic deno calls `TypedArray`: the shared prototype of every typed
// array constructor, which has no global name.
const TypedArray = ObjectGetPrototypeOf(Uint8Array);

// Captured at module load, before content runs: a primordial is a copy of an
// intrinsic taken while the intrinsics are still the platform's.
const INTRINSICS = new Map();
for (const name of ObjectGetOwnPropertyNames(globalThis)) {
  const descriptor = ObjectGetOwnPropertyDescriptor(globalThis, name);
  if (descriptor && "value" in descriptor && descriptor.value !== null) {
    const kind = typeof descriptor.value;
    if (kind === "function" || kind === "object") INTRINSICS.set(name, descriptor.value);
  }
}
INTRINSICS.set("TypedArray", TypedArray);
const INTRINSIC_NAMES = [...INTRINSICS.keys()].sort((a, b) => b.length - a.length);

// The Worker's own platform -- timers, the clock, the event target -- lives in
// `platform.mjs`, captured before this module installs the engine's globals and
// before content installs its own. Re-exported for the modules that take it
// from here.
export { platform };

const lowerFirst = (text) => text.charAt(0).toLowerCase() + text.slice(1);

// deno spells well-known symbols as `Symbol<Name>`: `SymbolIterator`,
// `SymbolToStringTag`.
function symbolFor(name) {
  const key = lowerFirst(name);
  const symbol = Symbol[key];
  return typeof symbol === "symbol" ? symbol : null;
}

const ReflectOwnKeys = Reflect.ownKeys;
const ReflectDefineProperty = Reflect.defineProperty;
const ObjectSetPrototypeOf = Object.setPrototypeOf;

function copyProps(source, target) {
  for (const key of ReflectOwnKeys(source)) {
    if (!ObjectGetOwnPropertyDescriptor(target, key)) {
      ReflectDefineProperty(target, key, ObjectGetOwnPropertyDescriptor(source, key));
    }
  }
}

/// An iterator whose `next` is the intrinsic's, captured now. deno's
/// `createSafeIterator`.
function createSafeIterator(factory, next) {
  class SafeIterator {
    constructor(iterable) {
      this._iterator = factory(iterable);
    }
    next() {
      return next(this._iterator);
    }
    [Symbol.iterator]() {
      return this;
    }
  }
  ObjectSetPrototypeOf(SafeIterator.prototype, null);
  ObjectFreeze(SafeIterator.prototype);
  ObjectFreeze(SafeIterator);
  return SafeIterator;
}

/// deno's `makeSafe`: the subclass's prototype holds the intrinsic methods
/// themselves, captured now, and is cut off from the intrinsic prototype, so
/// content replacing `Map.prototype.set` later changes nothing here. Methods
/// that return iterators return safe ones.
function makeSafe(unsafe, safe) {
  if (Symbol.iterator in unsafe.prototype) {
    const dummy = new unsafe();
    let next;
    for (const key of ReflectOwnKeys(unsafe.prototype)) {
      if (ObjectGetOwnPropertyDescriptor(safe.prototype, key)) continue;
      const descriptor = ObjectGetOwnPropertyDescriptor(unsafe.prototype, key);
      if (
        typeof descriptor.value === "function" &&
        descriptor.value.length === 0 &&
        Symbol.iterator in (ReflectApply(descriptor.value, dummy, []) ?? {})
      ) {
        const createIterator = uncurryThis(descriptor.value);
        next ??= uncurryThis(createIterator(dummy).next);
        const SafeIterator = createSafeIterator(createIterator, next);
        descriptor.value = function () {
          return new SafeIterator(this);
        };
      }
      ReflectDefineProperty(safe.prototype, key, descriptor);
    }
  } else {
    copyProps(unsafe.prototype, safe.prototype);
  }
  copyProps(unsafe, safe);
  ObjectSetPrototypeOf(safe.prototype, null);
  ObjectFreeze(safe.prototype);
  ObjectFreeze(safe);
  return safe;
}

const setIteratorNext = uncurryThis(ObjectGetPrototypeOf(new Set().values()).next);
const mapIteratorNext = uncurryThis(ObjectGetPrototypeOf(new Map().entries()).next);
const arrayIteratorNext = uncurryThis(ObjectGetPrototypeOf([].values()).next);

const SAFE = new Map([
  ["SafeMap", () => makeSafe(Map, class SafeMap extends Map {})],
  ["SafeSet", () => makeSafe(Set, class SafeSet extends Set {})],
  ["SafeWeakMap", () => makeSafe(WeakMap, class SafeWeakMap extends WeakMap {})],
  ["SafeWeakSet", () => makeSafe(WeakSet, class SafeWeakSet extends WeakSet {})],
  ["SafeWeakRef", () => makeSafe(WeakRef, class SafeWeakRef extends WeakRef {})],
  [
    "SafeFinalizationRegistry",
    () => makeSafe(FinalizationRegistry, class SafeFinalizationRegistry extends FinalizationRegistry {}),
  ],
  ["SafeSetIterator", () => createSafeIterator(uncurryThis(Set.prototype[Symbol.iterator]), setIteratorNext)],
  ["SafeMapIterator", () => createSafeIterator(uncurryThis(Map.prototype[Symbol.iterator]), mapIteratorNext)],
  ["SafeArrayIterator", () => createSafeIterator(uncurryThis(Array.prototype[Symbol.iterator]), arrayIteratorNext)],
]);

/// Resolve one primordials name by deno's naming rules, or throw.
///
///   Intrinsic                  the intrinsic itself            `Uint8Array`
///   IntrinsicStatic            a static member                 `ArrayIsArray`
///   IntrinsicPrototypeMethod   an uncurried prototype method   `MapPrototypeGet`
///   IntrinsicPrototypeGetProp  an uncurried prototype getter   `TypedArrayPrototypeGetLength`
///   Safe*                      a subclass immune to later prototype edits
export function resolvePrimordial(name) {
  const safe = SAFE.get(name);
  if (safe) return safe();

  for (const intrinsicName of INTRINSIC_NAMES) {
    if (!name.startsWith(intrinsicName)) continue;
    const rest = name.slice(intrinsicName.length);
    if (rest !== "" && !/^[A-Z]/.test(rest)) continue;
    const intrinsic = INTRINSICS.get(intrinsicName);
    if (rest === "") return intrinsic;

    if (rest.startsWith("Prototype") && intrinsic.prototype) {
      const member = rest.slice("Prototype".length);
      const prototype = intrinsic.prototype;
      if (member.startsWith("Get") || member.startsWith("Set")) {
        const property = member.slice(3);
        const key = property.startsWith("Symbol")
          ? symbolFor(property.slice("Symbol".length)) ?? lowerFirst(property)
          : lowerFirst(property);
        const descriptor = key === "" ? undefined : ObjectGetOwnPropertyDescriptor(prototype, key);
        const accessor = descriptor && (member.startsWith("Get") ? descriptor.get : descriptor.set);
        if (accessor) return uncurryThis(accessor);
      }
      const method = prototype[lowerFirst(member)];
      if (typeof method === "function") return uncurryThis(method);
      continue;
    }

    // Statics are copied as they are, as deno does: the ones the engine uses
    // (`ArrayIsArray`, `JSONParse`, `MathMax`, `ReflectApply`, ...) do not read
    // `this`, and a bound copy would add a frame to every call for nothing.
    const value = intrinsic[lowerFirst(rest)];
    if (value !== undefined) return value;
  }
  throw new TypeError(`primordials: no rule produces ${name}`);
}

/// The primordials object for the names the engine uses, frozen.
export function makePrimordials(names) {
  const primordials = { __proto__: null };
  for (const name of names) {
    primordials[name] = resolvePrimordial(name);
  }
  return ObjectFreeze(primordials);
}

/// A call that this profile does not offer. Stable, so content can catch it.
export class NotSupportedError extends Error {
  constructor(op, extension, profile) {
    super(`${op} is not available on ${profile} (${extension})`);
    this.name = "NotSupportedError";
    this.op = op;
    this.extension = extension;
    this.profile = profile;
  }
}

/// A call whose lane is decided (contracts/runtime/op-boundary.json) and whose
/// producer-side implementation has not landed. Distinct from NotSupportedError
/// because the answer to "will this ever work here" is the opposite.
export class LaneNotImplementedError extends Error {
  constructor(op, lane) {
    super(`${op} (${lane} lane) is not implemented on this producer yet`);
    this.name = "LaneNotImplementedError";
    this.op = op;
    this.lane = lane;
  }
}

export function notSupported(op, extension, profile) {
  return function unsupportedOp() {
    throw new NotSupportedError(op, extension, profile);
  };
}

export function notImplemented(op, lane) {
  return function unimplementedOp() {
    throw new LaneNotImplementedError(op, lane);
  };
}

// Brand checks, as deno's `core.is*` are: V8's internal type tests, which a
// value from another realm or with a replaced prototype still passes and an
// impostor with the right prototype does not. The intrinsic getters perform the
// same check -- they throw, or answer undefined, on anything without the slot.
const arrayBufferByteLength = uncurryThis(
  ObjectGetOwnPropertyDescriptor(ArrayBuffer.prototype, "byteLength").get,
);
const dataViewByteLength = uncurryThis(ObjectGetOwnPropertyDescriptor(DataView.prototype, "byteLength").get);
const typedArrayToStringTag = uncurryThis(
  ObjectGetOwnPropertyDescriptor(TypedArray.prototype, Symbol.toStringTag).get,
);
const sharedArrayBufferByteLength =
  typeof SharedArrayBuffer === "function"
    ? uncurryThis(ObjectGetOwnPropertyDescriptor(SharedArrayBuffer.prototype, "byteLength").get)
    : null;

function hasSlot(getter, value) {
  if (value === null || (typeof value !== "object" && typeof value !== "function")) return false;
  try {
    getter(value);
    return true;
  } catch {
    return false;
  }
}

const isArrayBuffer = (value) => hasSlot(arrayBufferByteLength, value);
const isDataView = (value) => hasSlot(dataViewByteLength, value);
const isTypedArray = (value) =>
  value !== null && typeof value === "object" && typedArrayToStringTag(value) !== undefined;
// No SharedArrayBuffer without cross-origin isolation, which the content origin
// does not have: nothing can be one.
const isSharedArrayBuffer = (value) =>
  sharedArrayBufferByteLength !== null && hasSlot(sharedArrayBufferByteLength, value);

/// The error classes the engine registers (`core.registerErrorClass`), by name.
///
/// Module scope rather than one map per `core`, because an op's error is built
/// where the host's answer arrives -- the service channel -- which has no `core`
/// of its own; a Worker has exactly one engine, so there is one registry.
const errorClasses = new Map();

/// The standard classes an op may name that the engine never registers.
const STANDARD_ERRORS = { Error, TypeError, RangeError, SyntaxError, ReferenceError, URIError, EvalError };

/**
 * An error of the class an op names, as deno_core builds one for a Rust op
 * that failed: the registered constructor for that name, a standard class, or
 * -- for a name nothing registered -- an `Error` carrying the name, so content
 * that reads `error.name` still sees it.
 */
export function constructOpError(className, message) {
  const Registered = errorClasses.get(className) ?? STANDARD_ERRORS[className];
  if (Registered !== undefined) return new Registered(message);
  const error = new Error(message);
  error.name = className;
  return error;
}

/// deno_core's `core`, for the members the engine uses.
///
/// `coreStream` carries the resource-table members (`core-stream.mjs`),
/// passed in rather than imported so this module keeps importing nothing:
/// it is what every other producer module bottoms out at.
export function makeCore(ops, coreStream) {
  const encoder = new TextEncoder();
  const decoder = new TextDecoder();
  const timers = new Map();
  let nextTimerId = 1;
  let timerDepth = 0;

  // deno keeps this in V8's continuation-preserved embedder data, which follows
  // promise jobs. The engine only saves and restores it around timer callbacks,
  // and nothing in it sets a context otherwise, so a slot is the same behaviour.
  let asyncContext;


  return ObjectFreeze({
    ops,
    isArrayBuffer,
    isDataView,
    isSharedArrayBuffer,
    isTypedArray,
    propNonEnumerable: (value) => ({ value, writable: true, enumerable: false, configurable: true }),
    propWritable: (value) => ({ value, writable: true, enumerable: true, configurable: true }),
    registerErrorClass(name, constructor) {
      errorClasses.set(name, constructor);
    },
    errorClass: (name) => errorClasses.get(name),
    encode: (text) => encoder.encode(text),
    decode: (bytes) => decoder.decode(bytes),

    // Timers ride the Worker's own. `depth` is the nesting the engine computed;
    // it is reported back by getTimerDepth while that timer's task runs, which
    // is how the engine applies the HTML nesting clamp itself.
    queueUserTimer(depth, repeat, timeout, task) {
      const id = nextTimerId++;
      const run = () => {
        const outer = timerDepth;
        timerDepth = depth;
        try {
          task();
        } finally {
          timerDepth = outer;
          if (!repeat) timers.delete(id);
        }
      };
      timers.set(id, { repeat, handle: repeat ? platform.setInterval(run, timeout) : platform.setTimeout(run, timeout) });
      return id;
    },
    cancelTimer(id) {
      const timer = timers.get(id);
      if (!timer) return;
      timers.delete(id);
      if (timer.repeat) platform.clearInterval(timer.handle);
      else platform.clearTimeout(timer.handle);
    },
    // A Worker has no event loop to keep alive or let exit: every timer is
    // "referenced" for as long as the Worker lives.
    refTimer() {},
    unrefTimer() {},
    getTimerDepth: () => timerDepth,
    getAsyncContext: () => asyncContext,
    setAsyncContext(context) {
      asyncContext = context;
    },

    setUnhandledPromiseRejectionHandler(handler) {
      platform.addEventListener("unhandledrejection", (event) => {
        if (handler(event.promise, event.reason)) event.preventDefault();
      });
    },
    setHandledPromiseRejectionHandler(handler) {
      platform.addEventListener("rejectionhandled", (event) => {
        handler(event.promise, event.reason);
      });
    },

    // The resource-table members. The handles they name are the network
    // service's, so the host answers them; `readAll` is built here out of
    // `read`. See core-stream.mjs and op-boundary.json's core_members.
    read: coreStream.read,
    readAll: coreStream.readAll,
    close: coreStream.close,
    tryClose: coreStream.tryClose,
  });
}

export { uncurryThis, ReflectApply as reflectApply, apply as functionApply };
