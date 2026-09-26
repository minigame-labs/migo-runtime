// The global scope content sees: the embedded runtime's, not a Worker's.
//
// Content runs in a WebKit Worker here, and a Worker publishes names the
// embedded runtime does not -- `importScripts`, `self`, `navigator`,
// `location`, `close`, `WorkerGlobalScope` and the rest. Engines read them to
// decide what they are running in: Phaser 3 sees `importScripts`, concludes it
// is in a Web Worker, reports neither Canvas nor WebGL and refuses to start --
// a black screen on the one platform where content runs in a Worker. So before
// content's first line, every own global the embedded runtime does not publish
// is retired, and the list of what it does publish is its committed surface
// baseline (`published_surface_full_v0.txt`, via `engine/published-globals.mjs`)
// rather than a list of Worker names someone thought of.
//
// A Worker's names are not all own properties: WebKit defines `importScripts`
// and the rest of WorkerGlobalScope's members on the interface prototypes
// between the global and EventTarget.prototype, and those are as visible to
// `typeof importScripts` as an own property. They are retired there too. The
// walk stops at EventTarget.prototype, which every event target shares -- the
// producer's own WebSocket among them -- and whose members stay reachable from
// the global because a global object's prototype chain cannot be changed.
// Everything the producer itself still needs from the Worker is captured in
// `platform.mjs` before this runs.

/**
 * Delete every string-named property not in `published` from `scope` and from
 * each object on its prototype chain before `shared` -- and never from
 * Object.prototype, whose members every object in the realm inherits. Answers the names that
 * could not be deleted; a non-configurable global is a surface this producer
 * cannot align, and the caller refuses to run content on it rather than run it
 * on a surface nobody has checked.
 */
export function retireUnpublishedGlobals(scope, published, shared) {
  const keep = new Set(published);
  const refused = [];
  for (
    let holder = scope;
    holder !== null && holder !== shared && holder !== Object.prototype;
    holder = Object.getPrototypeOf(holder)
  ) {
    for (const name of Object.getOwnPropertyNames(holder)) {
      if (keep.has(name)) continue;
      if (!Reflect.deleteProperty(holder, name)) refused.push(name);
    }
  }
  return refused;
}
