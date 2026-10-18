// Migo compatibility recorder: what a game actually reaches for while it runs.
//
// A boot prelude, injected after the adapter (if any) and before the game's entry. It
// is never part of a shipped host: off means not injected, so off adds nothing -- no
// global, no wrapper, no cost -- by construction rather than by a flag.
//
// It reports, as single marker-prefixed console lines a driver extracts from the log:
//   start      the recorder ran; a log without it was not recorded
//   call       the first call of each API, on `migo` or `wx`; a `wx` name for a function
//              migo publishes under another name carries migo's name too
//   absent     the first read or `in` test of each name a surface does not have
//              (feature detection, or a call about to throw)
//   installed  a name the content put on a surface itself -- its own shim, not ours
//   unrecorded a surface the recorder could not stand in for (an accessor, or frozen)
//   gl-extension / gl-error   what WebGL content asked for, and the errors it saw
//   first-frame  milliseconds from the recorder's start to the game's first
//              animation-frame callback
//
// Not among them, because the engine's log has them already and has them right: errors
// (an exception escaping a callback ends the game, "event loop error", and never reaches
// script; a listener on onUnhandledRejection would stop the game's own onError from
// hearing rejections), and the end of the entry's evaluation ("evaluate_module(...)",
// which waits for a top-level await that no timer set here can).
//
// What the game can observe of it, beyond those lines:
//   * a function read from a surface is the recorder's proxy of it, so `migo.setTimeout`
//     is no longer the global `setTimeout`;
//   * a surface's function is called on the surface object itself, as without the
//     recorder, so one that returns its receiver returns that object, not the proxy the
//     game holds;
//   * once content freezes a surface property, the language requires a read of it to
//     give the value itself, so it reads as the function it is, unrecorded.
// Everything else holds: arguments, `new` and subclassing reach the original, a
// function's own properties are its own, a function is the same every time it is read
// (and the same on `wx` as on `migo`, as the wx adapter promises), descriptors agree
// with reads, and a name that is absent is still absent. The recorder uses only
// intrinsics it took before content ran and objects with no prototype, so content that
// patches Object.prototype, JSON or a collection cannot make it throw or misreport.
//
// A function published under two names on one surface is reported under the name it was
// last read as, and a `new` under the surface it was last read from: one shared proxy
// per function is what keeps identity, and a call does not say which name it came by.
(function () {
  "use strict";

  var apply = Reflect.apply;
  var construct = Reflect.construct;
  var reflectGet = Reflect.get;
  var reflectHas = Reflect.has;
  var reflectDefine = Reflect.defineProperty;
  var ownDescriptor = Reflect.getOwnPropertyDescriptor;
  var ownNames = Object.getOwnPropertyNames;
  var create = Object.create;
  var hasOwn = Object.prototype.hasOwnProperty;
  var isPrototypeOf = Object.prototype.isPrototypeOf;
  var stringify = JSON.stringify;
  var round = Math.round;
  var ProxyCtor = Proxy;
  var WeakMapCtor = WeakMap;
  var weakGet = WeakMap.prototype.get;
  var weakSet = WeakMap.prototype.set;
  var consoleObject = console;
  var log = console.log;
  var performanceObject = performance;
  var now = performance.now;

  var MARK = "__MIGO_COMPAT__";
  var started = apply(now, performanceObject, []);
  var seen = create(null);

  function own(object, key) {
    return apply(hasOwn, object, [key]);
  }

  // An object with no prototype, from [key, value, ...]: JSON.stringify finds no toJSON on
  // it, and a Proxy finds no trap on it but the ones given.
  function make(pairs) {
    var made = create(null);
    for (var i = 0; i < pairs.length; i += 2) {
      made[pairs[i]] = pairs[i + 1];
    }
    return made;
  }

  // A descriptor as an object with no prototype: the language reads a descriptor's fields
  // through its prototype chain, and a content `Object.prototype.get` would otherwise turn
  // every value descriptor the recorder handles into an invalid one.
  function copy(descriptor) {
    var made = create(null);
    var fields = ["value", "writable", "get", "set", "enumerable", "configurable"];
    for (var i = 0; i < fields.length; i++) {
      if (own(descriptor, fields[i])) {
        made[fields[i]] = descriptor[fields[i]];
      }
    }
    return made;
  }

  function emit(pairs) {
    apply(log, consoleObject, [MARK + stringify(make(pairs))]);
  }

  function once(id, pairs) {
    if (seen[id] !== true) {
      seen[id] = true;
      emit(pairs);
    }
  }

  emit(["kind", "start", "version", 1]);

  // Before the surfaces are proxied, so each holder of the engine's requestAnimationFrame
  // -- the global, and `migo` and `wx`, which mirror it -- holds the observed one. A holder
  // whose property cannot be written keeps the engine's.
  var raf = globalThis.requestAnimationFrame;
  if (typeof raf === "function") {
    var framed = false;
    var observed = new ProxyCtor(raf, make(["apply", function (t, thisArg, args) {
      var callback = args[0];
      if (framed || typeof callback !== "function") {
        return apply(t, thisArg, args);
      }
      // The trap's own list of the arguments, nobody else's: replacing its first is
      // replacing an own element, which runs no setter content could have put on Array.
      args[0] = function () {
        if (!framed) {
          framed = true;
          emit(["kind", "first-frame", "ms", round((apply(now, performanceObject, []) - started) * 10) / 10]);
        }
        return apply(callback, this, arguments);
      };
      return apply(t, thisArg, args);
    }]));
    var holders = [globalThis, globalThis.migo, globalThis.wx];
    for (var h = 0; h < holders.length; h++) {
      var holder = holders[h];
      var held = typeof holder === "object" && holder !== null ? ownDescriptor(holder, "requestAnimationFrame") : undefined;
      if (held !== undefined && own(held, "value") && held.value === raf && held.writable === true) {
        reflectDefine(holder, "requestAnimationFrame", make(["value", observed]));
      }
    }
  }

  // migo's own name for each of its functions, taken before the game runs: an adapter
  // that publishes one under another name is reporting migo's API, and the report looks
  // it up by migo's name -- the one the API's spec and conformance use.
  var migoNames = new WeakMapCtor();
  if (typeof globalThis.migo === "object" && globalThis.migo !== null) {
    var keys = ownNames(globalThis.migo);
    for (var k = 0; k < keys.length; k++) {
      var published = ownDescriptor(globalThis.migo, keys[k]);
      if (own(published, "value") && typeof published.value === "function"
          && apply(weakGet, migoNames, [published.value]) === undefined) {
        apply(weakSet, migoNames, [published.value, keys[k]]);
      }
    }
  }

  // One proxy per original function, whichever surface and name it was read as: the wx
  // adapter publishes migo's own functions, and identity between the two must survive.
  // The proxy remembers the name each surface knows it by, so a call made through a
  // surface is reported under that surface's name.
  var surfaceOf = new WeakMapCtor();
  var entries = new WeakMapCtor();

  function recorded(original, surface, key) {
    var entry = apply(weakGet, entries, [original]);
    if (entry === undefined) {
      entry = make(["original", original, "names", create(null), "last", null, "proxy", null]);
      entry.proxy = new ProxyCtor(original, make([
        "apply", function (t, thisArg, args) {
          var via = apply(weakGet, surfaceOf, [thisArg]);
          called(entry, via === undefined ? undefined : via.name);
          return apply(t, via === undefined ? thisArg : via.target, args);
        },
        "construct", function (t, args, newTarget) {
          called(entry, undefined);
          // The original sees itself as new.target, as it would without the recorder.
          return construct(t, args, newTarget === entry.proxy ? t : newTarget);
        },
      ]));
      apply(weakSet, entries, [original, entry]);
    }
    entry.names[surface] = key;
    entry.last = surface;
    return entry.proxy;
  }

  function called(entry, via) {
    var surface = via !== undefined && entry.names[via] !== undefined ? via : entry.last;
    var api = entry.names[surface];
    var name = apply(weakGet, migoNames, [entry.original]);
    once("call:" + surface + "." + api, surface !== "migo" && name !== undefined && name !== api
      ? ["kind", "call", "surface", surface, "api", api, "migo", name]
      : ["kind", "call", "surface", surface, "api", api]);
  }

  function record(surface) {
    var held = ownDescriptor(globalThis, surface);
    if (held === undefined) {
      return;
    }
    // Standing in for the surface means replacing the global's value: an accessor would
    // lose its getter and setter, and a frozen global cannot be replaced at all.
    if (!own(held, "value") || (held.writable === false && held.configurable === false)) {
      emit(["kind", "unrecorded", "surface", surface, "reason", own(held, "value") ? "frozen" : "accessor"]);
      return;
    }
    var target = held.value;
    if (typeof target !== "object" || target === null) {
      return;
    }
    var installed = create(null);

    function noteAbsent(key) {
      once("absent:" + surface + "." + key, ["kind", "absent", "surface", surface, "api", key]);
    }

    // What a read of `key` hands the game. A proxy may not stand in for a frozen own
    // property: the language requires such a read to return the value itself.
    function shown(t, key, value) {
      if (typeof value !== "function" || typeof key !== "string" || installed[key] === true) {
        return value;
      }
      var current = ownDescriptor(t, key);
      if (current !== undefined && current.configurable === false && current.writable === false) {
        return value;
      }
      return recorded(value, surface, key);
    }

    var proxy;
    // No `set` trap: an assignment through the proxy ends in its defineProperty, so
    // `wx.foo = shim` and `Object.defineProperty(wx, "foo", ...)` are both seen there.
    proxy = new ProxyCtor(target, make([
      "get", function (t, key, receiver) {
        // Read as the surface itself, as without the recorder: a getter may check its receiver.
        var value = reflectGet(t, key, receiver === proxy ? t : receiver);
        if (value === undefined && typeof key === "string" && !reflectHas(t, key)) {
          noteAbsent(key);
        }
        return shown(t, key, value);
      },
      "has", function (t, key) {
        var present = reflectHas(t, key);
        if (!present && typeof key === "string") {
          noteAbsent(key);
        }
        return present;
      },
      "getOwnPropertyDescriptor", function (t, key) {
        var descriptor = ownDescriptor(t, key);
        if (descriptor === undefined) {
          return undefined;
        }
        var made = copy(descriptor);
        if (own(made, "value")) {
          made.value = shown(t, key, made.value);
        }
        return made;
      },
      "defineProperty", function (t, key, descriptor) {
        var defined = reflectDefine(t, key, copy(descriptor));
        // Only a definition that gives the name a value is the content's own: one that
        // changes attributes alone (Object.seal, `enumerable: false`) leaves the engine's.
        if (defined && typeof key === "string"
            && (own(descriptor, "value") || own(descriptor, "get") || own(descriptor, "set"))) {
          installed[key] = true;
          once("installed:" + surface + "." + key, ["kind", "installed", "surface", surface, "api", key]);
        }
        return defined;
      },
    ]));
    apply(weakSet, surfaceOf, [proxy, make(["name", surface, "target", target])]);
    reflectDefine(globalThis, surface, make(["value", proxy]));
  }

  record("wx");
  record("migo");

  // WebGL2RenderingContext extends WebGLRenderingContext here, so a method one context
  // inherits is observed once, on the prototype that owns it, and the context it was
  // called on says which version asked.
  var GL2 = globalThis.WebGL2RenderingContext;
  var gl2Prototype = typeof GL2 === "function" ? GL2.prototype : null;

  function version(context) {
    return gl2Prototype !== null && apply(isPrototypeOf, gl2Prototype, [context]) ? "webgl2" : "webgl";
  }

  function observe(proto, key, after) {
    if (!own(proto, key) || typeof proto[key] !== "function") {
      return;
    }
    proto[key] = new ProxyCtor(proto[key], make(["apply", function (t, thisArg, args) {
      var result = apply(t, thisArg, args);
      after(version(thisArg), args, result);
      return result;
    }]));
  }

  var contexts = [globalThis.WebGLRenderingContext, GL2];
  for (var c = 0; c < contexts.length; c++) {
    if (typeof contexts[c] !== "function") {
      continue;
    }
    observe(contexts[c].prototype, "getExtension", function (context, args, extension) {
      // Named as asked only when asked by a string: converting anything else again would
      // run the content's toString a second time.
      var name = typeof args[0] === "string" ? args[0] : "(" + typeof args[0] + ")";
      once("gl-extension:" + context + ":" + name,
        ["kind", "gl-extension", "context", context, "name", name, "supported", extension != null]);
    });
    observe(contexts[c].prototype, "getError", function (context, args, code) {
      if (code !== 0) {
        once("gl-error:" + context + ":" + code, ["kind", "gl-error", "context", context, "code", code]);
      }
    });
  }
})();
