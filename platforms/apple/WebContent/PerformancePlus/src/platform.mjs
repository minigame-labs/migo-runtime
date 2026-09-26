// The Worker's own platform, captured before anything else runs in it.
//
// The producer shares its global scope with two things that replace globals:
// the engine, which installs `setTimeout`, `performance`, `console` and more
// built on its own ops, and the content, whose web adapter installs its own
// `XMLHttpRequest`, `WebSocket` and `Image` over `migo.*`. Anything in the
// producer that read a global at call time would call one of those instead of
// the platform underneath it -- the engine calling its own implementation of
// itself, or the synchronous bridge posting through a content shim. The second
// shipped: every sync call a Pixi game made after its adapter loaded came back
// with status 0, because the adapter's `XMLHttpRequest` has no synchronous mode.
//
// So every producer module that needs a platform primitive takes it from here,
// and nothing below may read one from `globalThis` when it is used. This module
// imports nothing, so it is evaluated before any module that could install a
// global, and before the content, which runs after the whole graph.
//
// No DOM and no Node API beyond what both provide: it is also imported by
// modules that run under `node` for their tests, where `XMLHttpRequest` is
// absent and those tests inject their own transport.

const platformPerformance = globalThis.performance;

export const platform = Object.freeze({
  now: platformPerformance.now.bind(platformPerformance),
  timeOrigin: platformPerformance.timeOrigin,
  setTimeout: globalThis.setTimeout,
  clearTimeout: globalThis.clearTimeout,
  setInterval: globalThis.setInterval,
  clearInterval: globalThis.clearInterval,
  addEventListener: globalThis.addEventListener?.bind(globalThis),
  XMLHttpRequest: globalThis.XMLHttpRequest,
  fetch: globalThis.fetch?.bind(globalThis),
});
