// The recorder against a realm shaped like the engine's.
//
// Each check boots a fresh vm context the way a host boots a game: the engine's globals,
// `migo` mirroring them (97_migo_namespace.js copies descriptors as they are), `wx`
// mirroring `migo` under the wx adapter's names, then the recorder, then the game. The
// recorder's promise is two-sided -- it reports what the game reached for, and the game
// cannot tell it is there -- so most checks assert both halves.
//
// Run:  node tools/compat-recorder/recorder.test.mjs
// Gate: scripts/test-compat-recorder.sh

import { readFileSync } from "node:fs";
import vm from "node:vm";

const RECORDER = readFileSync(new URL("./recorder.js", import.meta.url), "utf8");
const MARK = "__MIGO_COMPAT__";

// Each in a block: a top-level const or class in a classic script is a global lexical
// binding, which would shadow the property the recorder replaces for every later script.
const ENGINE = `"use strict";
{
  globalThis.console = { log: function () { __host.lines.push(Array.prototype.join.call(arguments, " ")); } };
  globalThis.performance = { now: function () { return __host.now; } };
  globalThis.setTimeout = function (fn) { __host.timers.push(fn); return __host.timers.length; };
  globalThis.requestAnimationFrame = function requestAnimationFrame(callback) {
    __host.frames.push(callback);
    return __host.frames.length;
  };
  class WebGLRenderingContext {
    getExtension(name) { return name === "OES_vertex_array_object" ? {} : null; }
    getError() { return __host.glError; }
  }
  class WebGL2RenderingContext extends WebGLRenderingContext {}
  globalThis.WebGLRenderingContext = WebGLRenderingContext;
  globalThis.WebGL2RenderingContext = WebGL2RenderingContext;
  globalThis.request = function request(options) { return { options: options, receiver: this }; };
  globalThis.updateHostApp = function updateHostApp() {};
  globalThis.onError = function onError(listener) { __host.listeners.push(["onError", listener]); };
  globalThis.onUnhandledRejection = function onUnhandledRejection(listener) {
    __host.listeners.push(["onUnhandledRejection", listener]);
  };
  class Thing {
    constructor(x) { this.x = x; this.made = new.target; }
  }
  Thing.CONSTANT = 7;
  globalThis.Thing = Thing;
  __host.raw = { request: globalThis.request, Thing: Thing, raf: globalThis.requestAnimationFrame };

  const migo = {};
  for (const key of ["request", "updateHostApp", "onError", "onUnhandledRejection", "Thing",
                     "requestAnimationFrame", "setTimeout", "WebGLRenderingContext", "WebGL2RenderingContext"]) {
    Object.defineProperty(migo, key, Object.getOwnPropertyDescriptor(globalThis, key));
  }
  migo.nothing = undefined;
  __host.raw.frozen = function frozen() {};
  Object.defineProperty(migo, "frozen", { value: __host.raw.frozen, writable: false, configurable: false, enumerable: true });
  Object.defineProperty(globalThis, "migo", { value: migo, writable: true, enumerable: true, configurable: true });
}`;

// The wx adapter's shape: migo's descriptors as they are, a host-app API under the platform's name for it.
const ADAPTER = `"use strict";
{
  const names = { updateHostApp: "updatePlatformApp" };
  const wx = {};
  for (const key of Object.getOwnPropertyNames(migo)) {
    Object.defineProperty(wx, names[key] || key, Object.getOwnPropertyDescriptor(migo, key));
  }
  Object.defineProperty(globalThis, "wx", { value: wx, writable: true, enumerable: true, configurable: true });
  __host.rawWx = wx;
}`;

function boot({ adapter = ADAPTER } = {}) {
  const host = { lines: [], timers: [], frames: [], listeners: [], now: 1000, glError: 0 };
  const context = vm.createContext({ __host: host });
  vm.runInContext(ENGINE, context);
  if (adapter) {
    vm.runInContext(adapter, context);
  }
  const before = vm.runInContext("Object.getOwnPropertyNames(globalThis)", context);
  vm.runInContext(RECORDER, context, { filename: "recorder.js" });
  return {
    host,
    before,
    game: (source) => vm.runInContext(source, context),
    events: () => host.lines.filter((l) => l.startsWith(MARK)).map((l) => JSON.parse(l.slice(MARK.length))),
    of: (kind) => host.lines.filter((l) => l.startsWith(MARK)).map((l) => JSON.parse(l.slice(MARK.length)))
      .filter((e) => e.kind === kind),
  };
}

let failures = 0;

function check(name, fn) {
  try {
    fn();
    console.log(`  ok   ${name}`);
  } catch (error) {
    failures += 1;
    console.log(`  FAIL ${name}\n       ${error && error.stack ? error.stack.split("\n").slice(0, 3).join("\n       ") : error}`);
  }
}

function assertEqual(actual, expected, message) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) {
    throw new Error(`${message}: got ${a}, expected ${e}`);
  }
}

function assertTrue(value, message) {
  if (value !== true) {
    throw new Error(`${message}: got ${JSON.stringify(value)}`);
  }
}

check("the first line says the recorder ran, and nothing else is said before the game runs", () => {
  const run = boot();
  assertEqual(run.events(), [{ kind: "start", version: 1 }], "the lines at boot");
});

check("a call is reported once, under the surface it was made through", () => {
  const run = boot();
  run.game("wx.request({}); wx.request({}); migo.request({});");
  assertEqual(run.of("call"), [
    { kind: "call", surface: "wx", api: "request" },
    { kind: "call", surface: "migo", api: "request" },
  ], "the calls");
});

check("a function both surfaces share is reported under each surface's own name for it", () => {
  const run = boot();
  assertTrue(run.game("wx.updatePlatformApp === migo.updateHostApp"), "one function, two names");
  run.game("migo.updateHostApp(); wx.updatePlatformApp(); wx.request({});");
  assertEqual(run.of("call"), [
    { kind: "call", surface: "migo", api: "updateHostApp" },
    { kind: "call", surface: "wx", api: "updatePlatformApp", migo: "updateHostApp" },
    { kind: "call", surface: "wx", api: "request" },
  ], "the calls, each wx one naming migo's API where migo's name differs");
});

check("a call through a surface is that surface's even when another read came between", () => {
  // The function is read, then its arguments are evaluated -- reading it again, through
  // wx -- and only then called, on migo.
  const run = boot();
  run.game("migo.updateHostApp(wx.updatePlatformApp);");
  assertEqual(run.of("call"), [{ kind: "call", surface: "migo", api: "updateHostApp" }], "the call");
});

check("a function held apart from its surface is reported under the name it was last read as", () => {
  const run = boot();
  run.game("var held = migo.request; wx.updatePlatformApp; held({});");
  assertEqual(run.of("call"), [{ kind: "call", surface: "migo", api: "request" }], "the detached call");
});

check("a function is the same on every read, on both surfaces, and through its descriptor", () => {
  const run = boot();
  assertTrue(run.game("wx.request === wx.request && wx.request === migo.request"), "identity across reads");
  assertTrue(run.game("Object.getOwnPropertyDescriptor(wx, 'request').value === wx.request"), "the descriptor's value");
  assertTrue(run.game("var copy = {}; Object.defineProperty(copy, 'r', Object.getOwnPropertyDescriptor(wx, 'request')); copy.r({}); true"),
    "a copied descriptor");
  assertEqual(run.of("call"), [{ kind: "call", surface: "wx", api: "request" }], "a call through a copied descriptor");
});

check("a call through a surface reaches the surface itself, and any other receiver as given", () => {
  const run = boot();
  assertTrue(run.game("var o = {}; var r = wx.request(o); r.options === o && r.receiver === __host.rawWx"),
    "a method call is made on the object the adapter built, as without the recorder");
  assertTrue(run.game("var r = migo.request.call(42, 'x'); r.receiver === 42 && r.options === 'x'"), "an explicit receiver");
  assertTrue(run.game("var r = (0, wx.request)(); r.receiver === undefined"), "no receiver");
});

check("new, subclassing and a constructor's own properties reach the original", () => {
  const run = boot();
  assertTrue(run.game("var t = new wx.Thing(3); t.x === 3 && t instanceof wx.Thing && t.made === __host.raw.Thing"),
    "new through a surface");
  assertTrue(run.game("wx.Thing.CONSTANT === 7 && wx.Thing.name === 'Thing' && wx.Thing.prototype === __host.raw.Thing.prototype"),
    "the constructor's own properties");
  assertTrue(run.game("class Sub extends migo.Thing {} var s = new Sub(1); s.made === Sub && s instanceof migo.Thing"),
    "a subclass");
  assertEqual(run.of("call"), [
    { kind: "call", surface: "wx", api: "Thing" },
    { kind: "call", surface: "migo", api: "Thing" },
  ], "the constructions");
});

check("a name a surface lacks is reported once, read or tested, and stays absent", () => {
  const run = boot();
  assertTrue(run.game("wx.createVKSession === undefined && wx.createVKSession === undefined"), "a read");
  assertTrue(run.game("!('getGPU' in wx)"), "an in test");
  assertTrue(run.game("typeof migo.faceDetect === 'undefined'"), "a typeof");
  run.game("migo.nothing; 'request' in wx; wx[Symbol.iterator];");
  assertEqual(run.of("absent"), [
    { kind: "absent", surface: "wx", api: "createVKSession" },
    { kind: "absent", surface: "wx", api: "getGPU" },
    { kind: "absent", surface: "migo", api: "faceDetect" },
  ], "the absent names");
});

check("a name the content installs is reported, and what it installed is not recorded as ours", () => {
  const run = boot();
  run.game("if (!wx.shim) { wx.shim = function shim() { return 1; }; } wx.shim();");
  run.game("Object.defineProperty(migo, 'own', { value: function own() {}, configurable: true }); migo.own();");
  assertTrue(run.game("wx.shim.name === 'shim' && wx.shim === Object.getOwnPropertyDescriptor(wx, 'shim').value"),
    "the content's own function");
  assertEqual(run.of("installed"), [
    { kind: "installed", surface: "wx", api: "shim" },
    { kind: "installed", surface: "migo", api: "own" },
  ], "the installed names");
  assertEqual(run.of("call"), [], "no calls");
  assertEqual(run.of("absent"), [{ kind: "absent", surface: "wx", api: "shim" }], "the probe before the shim");
});

check("a definition the surface refuses installs nothing", () => {
  const run = boot();
  run.game("migo.frozen = function () {};");
  assertTrue(run.game("Reflect.defineProperty(migo, 'frozen', { value: function () {} }) === false"), "refused");
  assertEqual(run.of("installed"), [], "the refused assignment and definition");
});

check("a frozen own property reads as itself", () => {
  const run = boot();
  assertTrue(run.game("migo.frozen === __host.raw.frozen"), "the read");
  assertTrue(run.game("Object.getOwnPropertyDescriptor(migo, 'frozen').value === __host.raw.frozen"), "the descriptor");
});

check("the first animation frame is reported once, whichever holder asked for it", () => {
  for (const holder of ["globalThis", "migo", "wx", "GameGlobalAlias"]) {
    const run = boot();
    const call = holder === "GameGlobalAlias" ? "var g = globalThis; g.requestAnimationFrame" : `${holder}.requestAnimationFrame`;
    run.game(`var got = []; ${call}(function (t) { got.push(t); }); ${call}(function (t) { got.push(t); });`);
    assertEqual(run.of("first-frame"), [], `${holder}: nothing before a frame ran`);
    run.host.now = 1016.66;
    for (const callback of run.host.frames.splice(0)) {
      callback(16.66);
    }
    run.game(`${call}(function (t) { got.push(t); });`);
    run.host.frames.splice(0).forEach((callback) => callback(33.3));
    assertEqual(run.of("first-frame"), [{ kind: "first-frame", ms: 16.7 }], `${holder}: the first frame`);
    assertEqual(run.game("got"), [16.66, 16.66, 33.3], `${holder}: every callback ran with its timestamp`);
  }
});

check("requestAnimationFrame keeps its name and returns the engine's handle", () => {
  const run = boot();
  assertTrue(run.game("requestAnimationFrame.name === 'requestAnimationFrame' && requestAnimationFrame(function () {}) === 1"),
    "the global");
});

check("each WebGL extension asked for is reported once, with the context version and the answer", () => {
  const run = boot();
  run.game(`
    var gl2 = new WebGL2RenderingContext();
    var gl1 = new WebGLRenderingContext();
    gl2.getExtension("OES_vertex_array_object");
    gl2.getExtension("OES_vertex_array_object");
    gl1.getExtension("OES_vertex_array_object");
    gl1.getExtension("WEBGL_compressed_texture_astc");
  `);
  assertEqual(run.of("gl-extension"), [
    { kind: "gl-extension", context: "webgl2", name: "OES_vertex_array_object", supported: true },
    { kind: "gl-extension", context: "webgl", name: "OES_vertex_array_object", supported: true },
    { kind: "gl-extension", context: "webgl", name: "WEBGL_compressed_texture_astc", supported: false },
  ], "the extensions");
  assertTrue(run.game("WebGLRenderingContext.prototype.getExtension.name === 'getExtension'"), "the method's name");
  assertTrue(run.game("!Object.prototype.hasOwnProperty.call(WebGL2RenderingContext.prototype, 'getExtension')"),
    "the inherited method stays inherited");
});

check("a WebGL error is reported once per code, and no error is not one", () => {
  const run = boot();
  run.game("var gl = new WebGLRenderingContext(); gl.getError();");
  run.host.glError = 1282;
  assertEqual(run.game("gl.getError() + gl.getError()"), 2564, "the codes reach the game");
  assertEqual(run.of("gl-error"), [{ kind: "gl-error", context: "webgl", code: 1282 }], "the errors");
});

check("the recorder listens for no errors: a listener would change what the game's onError hears", () => {
  const run = boot();
  assertEqual(run.host.listeners, [], "listeners registered");
});

check("the recorder adds no global name and keeps each surface's attributes", () => {
  const run = boot();
  assertEqual(run.game("Object.getOwnPropertyNames(globalThis)"), run.before, "the global names");
  assertEqual(run.game("var d = Object.getOwnPropertyDescriptor(globalThis, 'wx'); [d.writable, d.enumerable, d.configurable]"),
    [true, true, true], "the wx descriptor");
});

check("without an adapter there is no wx, and migo is still recorded", () => {
  const run = boot({ adapter: null });
  assertTrue(run.game("!('wx' in globalThis)"), "wx");
  run.game("migo.request({});");
  assertEqual(run.of("call"), [{ kind: "call", surface: "migo", api: "request" }], "the call");
});

check("every line is one marker and one JSON object", () => {
  const run = boot();
  run.game("wx['line\\nbreak'];");
  const marked = run.host.lines.filter((l) => l.includes(MARK));
  assertTrue(marked.every((l) => l.startsWith(MARK) && !l.includes("\n")), "one line each");
  assertEqual(run.of("absent"), [{ kind: "absent", surface: "wx", api: "line\nbreak" }], "the name survives");
});

check("a surface whose methods and getters check their receiver's brand still works", () => {
  const run = boot({ adapter: `"use strict";
{
  class Surface {
    #secret = 7;
    secret() { return this.#secret; }
    get version() { return this.#secret; }
  }
  Object.defineProperty(globalThis, "wx", { value: new Surface(), writable: true, enumerable: true, configurable: true });
}` });
  assertTrue(run.game("wx.secret() === 7 && wx.version === 7"), "private fields reached through the surface");
  assertEqual(run.of("call"), [{ kind: "call", surface: "wx", api: "secret" }], "the call");
});

check("content that patches the intrinsics after boot cannot make the recorder throw or misreport", () => {
  const run = boot();
  const result = run.game(`
    var thrower = function () { throw new Error("patched"); };
    var saved = { toJSON: Object.prototype.toJSON, stringify: JSON.stringify, weakGet: WeakMap.prototype.get,
                  weakSet: WeakMap.prototype.set, apply: Reflect.apply, defineProperty: Reflect.defineProperty,
                  getOwn: Reflect.getOwnPropertyDescriptor, has: Object.prototype.hasOwnProperty,
                  isProto: Object.prototype.isPrototypeOf, round: Math.round, create: Object.create };
    Object.prototype.toJSON = thrower;
    Object.defineProperty(Object.prototype, "get", { value: function () { return 1; }, writable: true, configurable: true });
    Object.defineProperty(Array.prototype, "6", { set: thrower, configurable: true });
    JSON.stringify = thrower; WeakMap.prototype.get = thrower; WeakMap.prototype.set = thrower;
    Reflect.apply = thrower; Reflect.defineProperty = thrower; Reflect.getOwnPropertyDescriptor = thrower;
    Object.prototype.hasOwnProperty = thrower; Object.prototype.isPrototypeOf = thrower; Math.round = thrower;
    Object.create = thrower;
    WebGL2RenderingContext.prototype.isPrototypeOf = null;
    var out = [];
    try {
      wx.request({});
      wx.updatePlatformApp();
      wx.shim = function () {};
      out.push(Object.getOwnPropertyDescriptor(wx, "request").value === wx.request);
      out.push(wx.nothingHere === undefined);
      __host.glError = 1282;
      out.push(new WebGL2RenderingContext().getError() === 1282);
      requestAnimationFrame(function () {});
      out.push("ok");
    } catch (error) {
      out.push("threw: " + error.message);
    }
    delete Object.prototype.toJSON; delete Object.prototype.get; delete Array.prototype["6"];
    JSON.stringify = saved.stringify; WeakMap.prototype.get = saved.weakGet; WeakMap.prototype.set = saved.weakSet;
    Reflect.apply = saved.apply; Reflect.defineProperty = saved.defineProperty; Reflect.getOwnPropertyDescriptor = saved.getOwn;
    Object.prototype.hasOwnProperty = saved.has; Object.prototype.isPrototypeOf = saved.isProto; Math.round = saved.round;
    Object.create = saved.create;
    delete WebGL2RenderingContext.prototype.isPrototypeOf;
    out.join(",");
  `);
  assertEqual(result, "true,true,true,ok", "the game's calls");
  run.host.frames.splice(0).forEach((callback) => callback(16));
  assertEqual(run.events().map((e) => e.kind + ":" + (e.api || e.context || "")),
    ["start:", "call:request", "call:updatePlatformApp", "installed:shim", "absent:nothingHere", "gl-error:webgl2",
     "first-frame:"], "what was recorded");
});

check("a holder whose requestAnimationFrame cannot be written keeps the engine's, and boot goes on", () => {
  const run = boot({ adapter: ADAPTER + `
    Object.defineProperty(wx, "requestAnimationFrame", { writable: false });` });
  assertTrue(run.game("Object.getOwnPropertyDescriptor(__host.rawWx, 'requestAnimationFrame').value === __host.raw.raf"),
    "the holder still holds the engine's function");
  assertEqual(run.game("wx.requestAnimationFrame(function () {})"), 1, "which still answers through the surface");
  run.game("requestAnimationFrame(function () {});");
  run.host.frames.splice(0).forEach((callback) => callback(16));
  assertEqual(run.of("first-frame").length, 1, "the global's frame is still seen");
});

check("a global surface that is an accessor, or frozen, is left alone and said to be", () => {
  const accessor = boot({ adapter: `"use strict";
{
  const wx = { request: function request() { return 1; } };
  Object.defineProperty(globalThis, "wx", { get: function () { __host.reads = (__host.reads || 0) + 1; return wx; },
    enumerable: true, configurable: true });
}` });
  assertTrue(accessor.game("wx.request() === 1 && typeof Object.getOwnPropertyDescriptor(globalThis, 'wx').get === 'function'"),
    "the accessor is still an accessor");
  assertTrue(accessor.host.reads >= 1, "and its getter still runs");
  assertEqual(accessor.of("unrecorded"), [{ kind: "unrecorded", surface: "wx", reason: "accessor" }], "the accessor");
  const frozen = boot({ adapter: `"use strict";
  Object.defineProperty(globalThis, "wx", { value: { request: function () {} }, writable: false, enumerable: true, configurable: false });` });
  assertEqual(frozen.of("unrecorded"), [{ kind: "unrecorded", surface: "wx", reason: "frozen" }], "the frozen global");
  frozen.game("migo.request({});");
  assertEqual(frozen.of("call"), [{ kind: "call", surface: "migo", api: "request" }], "migo is recorded still");
});

check("an extension asked for by something other than a string is converted once, by the context alone", () => {
  const run = boot();
  assertEqual(run.game(`var conversions = 0;
    new WebGLRenderingContext().getExtension({ toString: function () { conversions += 1; return "X"; } });
    conversions`), 0, "the recorder converted the name");
  assertEqual(run.of("gl-extension"), [{ kind: "gl-extension", context: "webgl", name: "(object)", supported: false }],
    "the extension");
});

check("a definition that changes attributes alone leaves the engine's function recorded", () => {
  const run = boot();
  assertTrue(run.game(`var before = migo.request;
    Object.defineProperty(migo, "request", { enumerable: false });
    Object.seal(migo);
    migo.request === before`), "the same function after");
  run.game("migo.request({});");
  assertEqual(run.of("installed"), [], "nothing installed");
  assertEqual(run.of("call"), [{ kind: "call", surface: "migo", api: "request" }], "the call");
});

check("the recorder is ASCII, so any host can inject it as it stands", () => {
  assertTrue(/^[\x00-\x7f]*$/.test(RECORDER), "ASCII");
});

console.log(failures === 0 ? "PASS" : `FAIL: ${failures} check(s) failed`);
process.exit(failures === 0 ? 0 : 1);
