// Migo compatibility recorder: what a game actually reaches for while it runs.
//
// A boot prelude, injected after the adapter (if any) and before the game's entry. It
// is never part of a shipped host: off means not injected, so off adds nothing -- no
// global, no wrapper, no cost -- by construction rather than by a flag.
//
// It reports, as single marker-prefixed console lines a driver extracts from the log:
//   start      the recorder ran; a log without it was not recorded
//   call       the first call of each API, on `migo` or `wx`
//   absent     the first read or `in` test of each name a surface does not have
//              (feature detection, or a call about to throw)
//   installed  a name the content put on a surface itself -- its own shim, not ours
//   gl-extension / gl-error   what WebGL content asked for, and the errors it saw
//   ready      milliseconds from the start of the game's scripts to the end of the
//              entry module's synchronous evaluation (a module still awaiting at top
//              level has not finished then)
//   first-frame  milliseconds to the game's first animation-frame callback
//
// Errors are not among them: the engine writes both kinds to the same log already.
// An exception escaping a callback ends the game ("event loop error") and never
// reaches script, and a listener on onUnhandledRejection would stop the game's own
// onError from hearing rejections -- the engine tells onError only when nobody
// listens for them.
//
// It changes nothing a game can observe beyond those lines, with one exception: a
// function read from a surface is the recorder's proxy of it, so `migo.setTimeout` is
// no longer the global `setTimeout`. Everything else holds: a call gets exactly the
// receiver and arguments it was given, `new` and subclassing reach the original, a
// function's own properties are its own, a function is the same every time it is read
// (and the same on `wx` as on `migo`, as the wx adapter promises), and a name that is
// absent is still absent.
(function () {
  "use strict";

  var MARK = "__MIGO_COMPAT__";
  var log = console.log;
  var clock = performance.now.bind(performance);
  var started = clock();
  var seen = new Set();

  function emit(event) {
    log(MARK + JSON.stringify(event));
  }

  function once(id, event) {
    if (!seen.has(id)) {
      seen.add(id);
      emit(event);
    }
  }

  function elapsed() {
    return Math.round((clock() - started) * 10) / 10;
  }

  emit({ kind: "start", version: 1 });

  // Before the surfaces are proxied, so each holder of the engine's requestAnimationFrame
  // -- the global, and `migo` and `wx`, which mirror it -- holds the observed one.
  var raf = globalThis.requestAnimationFrame;
  if (typeof raf === "function") {
    var framed = false;
    var observed = new Proxy(raf, {
      apply: function (t, thisArg, args) {
        var callback = args[0];
        if (framed || typeof callback !== "function") {
          return Reflect.apply(t, thisArg, args);
        }
        var rest = Array.prototype.slice.call(args, 1);
        return Reflect.apply(t, thisArg, [function () {
          if (!framed) {
            framed = true;
            emit({ kind: "first-frame", ms: elapsed() });
          }
          return Reflect.apply(callback, this, arguments);
        }].concat(rest));
      },
    });
    [globalThis, globalThis.migo, globalThis.wx].forEach(function (holder) {
      if (holder && holder.requestAnimationFrame === raf) {
        holder.requestAnimationFrame = observed;
      }
    });
  }

  // One proxy per original function, whichever surface and name it was read as: the wx
  // adapter publishes migo's own functions, and identity between the two must survive.
  // The proxy remembers the name each surface knows it by (an adapter publishes
  // migo.updateHostApp under its platform's own name for the host app), so a call made
  // through a surface is reported under that surface's name; a call made on a function
  // held apart from its surface is reported under the name it was last read as.
  var surfaceOf = new WeakMap();
  var entries = new WeakMap();

  function recorded(original, surface, key) {
    var entry = entries.get(original);
    if (entry === undefined) {
      entry = { names: new Map(), last: null, proxy: null };
      entry.proxy = new Proxy(original, {
        apply: function (t, thisArg, args) {
          called(entry, surfaceOf.get(thisArg));
          return Reflect.apply(t, thisArg, args);
        },
        construct: function (t, args, newTarget) {
          called(entry, undefined);
          // The original sees itself as new.target, as it would without the recorder.
          return Reflect.construct(t, args, newTarget === entry.proxy ? t : newTarget);
        },
      });
      entries.set(original, entry);
    }
    entry.names.set(surface, key);
    entry.last = surface;
    return entry.proxy;
  }

  function called(entry, via) {
    var surface = via !== undefined && entry.names.has(via) ? via : entry.last;
    var api = entry.names.get(surface);
    once("call:" + surface + "." + api, { kind: "call", surface: surface, api: api });
  }

  function record(surface) {
    var target = globalThis[surface];
    if (typeof target !== "object" || target === null) {
      return;
    }
    var installed = new Set();

    function noteAbsent(key) {
      once("absent:" + surface + "." + key, { kind: "absent", surface: surface, api: key });
    }

    // What a read of `key` hands the game. A proxy may not stand in for a frozen own
    // property: the language requires such a read to return the value itself.
    function shown(t, key, value) {
      if (typeof value !== "function" || typeof key !== "string" || installed.has(key)) {
        return value;
      }
      var own = Reflect.getOwnPropertyDescriptor(t, key);
      if (own !== undefined && own.configurable === false && own.writable === false) {
        return value;
      }
      return recorded(value, surface, key);
    }

    // No `set` trap: an assignment through the proxy ends in its defineProperty, so
    // `wx.foo = shim` and `Object.defineProperty(wx, "foo", ...)` are both seen there.
    var proxy = new Proxy(target, {
      get: function (t, key, receiver) {
        var value = Reflect.get(t, key, receiver);
        if (value === undefined && typeof key === "string" && !(key in t)) {
          noteAbsent(key);
        }
        return shown(t, key, value);
      },
      has: function (t, key) {
        var present = Reflect.has(t, key);
        if (!present && typeof key === "string") {
          noteAbsent(key);
        }
        return present;
      },
      getOwnPropertyDescriptor: function (t, key) {
        var descriptor = Reflect.getOwnPropertyDescriptor(t, key);
        if (descriptor !== undefined && "value" in descriptor) {
          descriptor.value = shown(t, key, descriptor.value);
        }
        return descriptor;
      },
      defineProperty: function (t, key, descriptor) {
        var defined = Reflect.defineProperty(t, key, descriptor);
        if (defined && typeof key === "string") {
          installed.add(key);
          once("installed:" + surface + "." + key, { kind: "installed", surface: surface, api: key });
        }
        return defined;
      },
    });
    surfaceOf.set(proxy, surface);
    var held = Object.getOwnPropertyDescriptor(globalThis, surface);
    Object.defineProperty(globalThis, surface, {
      value: proxy,
      writable: held.writable !== false,
      enumerable: held.enumerable,
      configurable: held.configurable,
    });
  }

  record("wx");
  record("migo");

  // WebGL2RenderingContext extends WebGLRenderingContext here, so a method one context
  // inherits is observed once, on the prototype that owns it, and the context it was
  // called on says which version asked.
  var GL2 = globalThis.WebGL2RenderingContext;

  function version(context) {
    return typeof GL2 === "function" && GL2.prototype.isPrototypeOf(context) ? "webgl2" : "webgl";
  }

  function observe(proto, key, after) {
    if (!Object.prototype.hasOwnProperty.call(proto, key) || typeof proto[key] !== "function") {
      return;
    }
    proto[key] = new Proxy(proto[key], {
      apply: function (t, thisArg, args) {
        var result = Reflect.apply(t, thisArg, args);
        after(version(thisArg), args, result);
        return result;
      },
    });
  }

  [globalThis.WebGLRenderingContext, GL2].forEach(function (Context) {
    if (typeof Context !== "function") {
      return;
    }
    observe(Context.prototype, "getExtension", function (context, args, extension) {
      var name = String(args[0]);
      once("gl-extension:" + context + ":" + name,
        { kind: "gl-extension", context: context, name: name, supported: extension != null });
    });
    observe(Context.prototype, "getError", function (context, args, code) {
      if (code !== 0) {
        once("gl-error:" + context + ":" + code, { kind: "gl-error", context: context, code: code });
      }
    });
  });

  // A timer set here runs once the entry module's synchronous evaluation is over: the
  // host keeps the event loop turning while it waits for the module to settle.
  setTimeout(function () {
    emit({ kind: "ready", ms: elapsed() });
  }, 0);
})();
