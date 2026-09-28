// Tasks that reach this Worker while it is blocked in a synchronous call wait
// until the call has returned and the script that made it has finished.
//
// A synchronous call (`SyncCaller`, sync-call.mjs) blocks in a synchronous
// XMLHttpRequest, and WebKit serves a Worker's synchronous request from a
// nested run loop that keeps firing the Worker's timers while the request is
// outstanding: on an iPhone XS Max (iOS 18.7), a zero-delay timer armed before
// 300 ms of back-to-back calls ran inside them, every time
// (MigoFrameAcceptanceTests, testNothingContentQueuedRunsInsideItsSynchronousCalls).
// A task run there runs inside the script that made the call, which
// JavaScript promises cannot happen: a timer fires between two statements of a
// function that never yielded. The same run saw no WebSocket event inside a
// call, but WebKit's source says its timers do not fire there either, so the
// producer holds both rather than trust either to the source. The synchronous path was designed on `Atomics.wait`,
// which dispatches nothing, and everything above it assumes that. What broke
// first was a Phaser game on an iPhone XS Max: the web adapter queues its
// `load` event on a zero-delay timer, and in the launches that went black that
// event had already fired when the game's bundle -- still being evaluated,
// blocked in synchronous calls on the way -- registered for it. The game never
// started: a black screen over a running frame loop, in about one launch in
// four under load.
//
// So every task source the producer owns enters JavaScript through `runTask`.
// While a synchronous call is outstanding the task is queued instead; once the
// outermost call returns, the queue drains one task per turn of the event loop,
// in arrival order, so each runs after the script that made the call and gets
// the microtask checkpoint a task gets. A task that arrives while the queue is
// still draining joins the back of it, so nothing overtakes what was queued
// first -- downlink messages carry order the producer relies on.
//
// Draining is a MessageChannel message rather than a zero-delay timer, because
// HTML clamps a timer scheduled from a timer's callback to 4 ms after five
// levels of nesting, and a frame-clock tick queued behind a readback must not
// wait on a clamp.

import { platform } from "./platform.mjs";

let depth = 0;
const queue = [];
let posted = false;
let port = null;

function post() {
  if (posted || depth > 0 || queue.length === 0) return;
  if (port === null) {
    const channel = new platform.MessageChannel();
    channel.port1.onmessage = runNext;
    // Node keeps its event loop alive for a port with a listener; a Worker
    // has no such notion and no `unref`.
    channel.port1.unref?.();
    port = channel.port2;
  }
  posted = true;
  port.postMessage(null);
}

function runNext() {
  posted = false;
  // A call began since this was posted -- this message was itself dispatched
  // by that call's nested loop. Its return posts again.
  if (depth > 0) return;
  const task = queue.shift();
  if (task === undefined) return;
  try {
    task();
  } finally {
    post();
  }
}

/**
 * Run `task` now, or after the synchronous call in progress -- and after every
 * task already waiting -- if there is one. An exception propagates to the
 * caller when the task runs now, and is reported as an uncaught exception of
 * its own turn when it runs later, as it would be from a task.
 */
export function runTask(task) {
  if (depth === 0 && queue.length === 0) {
    task();
    return;
  }
  queue.push(task);
  post();
}

/** Make a blocking call, holding every task that arrives meanwhile. */
export function synchronously(call) {
  depth += 1;
  try {
    return call();
  } finally {
    depth -= 1;
    post();
  }
}
