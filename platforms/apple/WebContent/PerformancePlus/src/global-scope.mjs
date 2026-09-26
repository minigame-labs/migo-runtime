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
// Inherited members stay reachable -- EventTarget's `addEventListener` among
// them -- because a global object's prototype chain is immutable. Everything the
// producer itself still needs from the Worker is captured in `platform.mjs`
// before this runs.

/**
 * Delete every own string-named property of `scope` not in `published`.
 * Answers the names that could not be deleted; a non-configurable global is a
 * surface this producer cannot align, and the caller refuses to run content on
 * it rather than run it on a surface nobody has checked.
 */
export function retireUnpublishedGlobals(scope, published) {
  const keep = new Set(published);
  const refused = [];
  for (const name of Object.getOwnPropertyNames(scope)) {
    if (keep.has(name)) continue;
    if (!Reflect.deleteProperty(scope, name)) refused.push(name);
  }
  return refused;
}
