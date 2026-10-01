// `onended` for the scheduled source nodes (AudioBufferSourceNode, OscillatorNode, ConstantSourceNode).
//
// The audio graph knows when a source finishes -- it ran out of buffer, or stop() came due -- and reports it for the nodes
// content asked about (`op_audio_watch_source_ended`), so a game that never sets `onended` costs the audio thread nothing. The
// report arrives as the host-bridge hook `_internalTriggerAudioSourceEnded(nodeId)`. Sound libraries chain on it: Phaser's
// `complete`, three.js's `onEnded`, Babylon's and PlayCanvas's sound end events.
//
// The registry holds weak references: a node content dropped must not be pinned by a handler that will never be heard.

import { op_audio_watch_source_ended } from "ext:core/ops";
import { createCallbackEvent, errorToString } from "ext:host_v8_base/02_async.js";

// nodeId -> WeakRef(node), for the nodes with an `onended` handler.
const watched = new Map();
const forget = new FinalizationRegistry((nodeId) => {
  const ref = watched.get(nodeId);
  if (ref !== undefined && ref.deref() === undefined) watched.delete(nodeId);
});

/// Called by a source node when its `onended` is assigned: starts or stops the native watch as the handler appears or goes.
function watchSourceEnded(node, handler) {
  const nodeId = node._nodeId;
  if (typeof handler === "function") {
    if (watched.has(nodeId)) return;
    watched.set(nodeId, new WeakRef(node));
    forget.register(node, nodeId);
    op_audio_watch_source_ended(nodeId, true);
  } else if (watched.delete(nodeId)) {
    op_audio_watch_source_ended(nodeId, false);
  }
}

/// The host-bridge hook: the node ended. Fires once; the watch is gone with it.
function _internalTriggerAudioSourceEnded(nodeId) {
  const ref = watched.get(nodeId);
  if (ref === undefined) return;
  watched.delete(nodeId);
  const node = ref.deref();
  if (node === undefined) return;
  const handler = node.onended;
  if (typeof handler !== "function") return;
  try {
    handler.call(node, createCallbackEvent("ended", node));
  } catch (e) {
    try {
      console.error(`AudioScheduledSourceNode onended error: ${errorToString(e)}`);
    } catch (_) {}
  }
}

export { watchSourceEnded, _internalTriggerAudioSourceEnded };
