import {
  op_audio_connect,
  op_audio_disconnect,
  op_audio_release_node,
} from "ext:core/ops";
 
const WARNED_CAPABILITIES = new Set();
function warnUnsupportedCapability(key, message) {
  if (WARNED_CAPABILITIES.has(key)) return;
  WARNED_CAPABILITIES.add(key);
  console.warn(`[MigoAudio] ${message}`);
}

// A native release is fire-and-forget and idempotent, but the audio command
// queue is bounded, and a *dropped* release is exactly the leak these finalizers
// exist to prevent. So a rejected send is retried with backoff.
//
// Only the numbers needed to name the resource are retained, never a node, a
// buffer or a context -- a queued retry that held the object would keep alive
// the very thing it is trying to free.
const INITIAL_RELEASE_RETRY_MS = 4;
const MAX_RELEASE_RETRY_MS = 1000;

/// Build a retrying, de-duplicating release queue over one native release op.
function createReleaseQueue(release) {
  const pending = new Map();
  let timer = null;
  let retryMs = INITIAL_RELEASE_RETRY_MS;

  function schedule() {
    if (timer !== null || pending.size === 0) return;
    timer = setTimeout(flush, retryMs);
  }

  function flush() {
    timer = null;
    for (const [key, argument] of pending) {
      try {
        release(argument);
        pending.delete(key);
      } catch {
        // Queue saturation is transient; keep the numeric key and retry.
      }
    }
    if (pending.size === 0) {
      retryMs = INITIAL_RELEASE_RETRY_MS;
      return;
    }
    retryMs = Math.min(retryMs * 2, MAX_RELEASE_RETRY_MS);
    schedule();
  }

  return function enqueueRelease(key, argument) {
    if (pending.has(key)) return;
    try {
      release(argument);
    } catch {
      pending.set(key, argument);
      schedule();
    }
  };
}

const releaseNativeNode = createReleaseQueue(({ ctxId, nodeId }) =>
  op_audio_release_node(ctxId, nodeId)
);

// Unreachable nodes must tell the audio thread, or every effect node a game
// creates -- one gain per sound effect is the ordinary shape -- stays in the
// graph for the context's whole life, along with its render buffer, and gets
// processed every quantum forever.
//
// Registering rather than tracking: a strong module-global map of nodes would
// pin every node and its context, which is what
// `web_audio_registries_do_not_strongly_retain_contexts_buffers_or_nodes`
// forbids. The registry holds a `{ctxId, nodeId}` record of numbers only.
const NODE_FINALIZER = new FinalizationRegistry((held) => {
  releaseNativeNode(held.nodeId, held);
});

class AudioNode {
  #context;
  #nodeId;
  #connections = [];
  #numberOfInputs;
  #numberOfOutputs;
  #channelCount;
  #channelCountMode;
  #channelInterpretation;

  constructor(context, nodeId, options = {}) {
    this.#context = context;
    this.#nodeId = nodeId;
    this.#numberOfInputs = options.numberOfInputs ?? 1;
    this.#numberOfOutputs = options.numberOfOutputs ?? 1;
    this.#channelCount = options.channelCount ?? 2;
    this.#channelCountMode = options.channelCountMode ?? "max";
    this.#channelInterpretation = options.channelInterpretation ?? "speakers";
    // The destination is owned by its context and has no native id of its own to
    // release; the native side ignores it, and registering it would only add a
    // finalizer that fires when the context is already being torn down.
    if (nodeId !== 0) {
      NODE_FINALIZER.register(this, {
        ctxId: context._nativeId,
        nodeId,
      });
    }
  }

  get context() {
    return this.#context;
  }

  get _nodeId() {
    return this.#nodeId;
  }

  get numberOfInputs() {
    return this.#numberOfInputs;
  }

  get numberOfOutputs() {
    return this.#numberOfOutputs;
  }

  get channelCount() {
    return this.#channelCount;
  }

  set channelCount(value) {
    this.#channelCount = value;
    warnUnsupportedCapability(
      "channel-metadata",
      "AudioNode channel metadata is stored in JS but not applied by the native graph",
    );
  }

  get channelCountMode() {
    return this.#channelCountMode;
  }

  set channelCountMode(value) {
    this.#channelCountMode = value;
    warnUnsupportedCapability(
      "channel-metadata",
      "AudioNode channel metadata is stored in JS but not applied by the native graph",
    );
  }

  get channelInterpretation() {
    return this.#channelInterpretation;
  }

  set channelInterpretation(value) {
    this.#channelInterpretation = value;
    warnUnsupportedCapability(
      "channel-metadata",
      "AudioNode channel metadata is stored in JS but not applied by the native graph",
    );
  }

  connect(destination, outputIndex = 0, inputIndex = 0) {
    if (!(destination instanceof AudioNode)) {
      throw new TypeError("destination must be an AudioNode");
    }
    if (outputIndex < 0 || outputIndex >= this.#numberOfOutputs) {
      throw new DOMException("outputIndex is out of range", "IndexSizeError");
    }
    if (inputIndex < 0 || inputIndex >= destination.numberOfInputs) {
      throw new DOMException("inputIndex is out of range", "IndexSizeError");
    }
    op_audio_connect(this.#nodeId, destination._nodeId, outputIndex, inputIndex);
    this.#connections.push(destination);
    return destination;
  }

  disconnect(destination) {
    if (destination !== undefined) {
      warnUnsupportedCapability(
        "targeted-disconnect",
        "targeted disconnect currently falls back to disconnecting all outputs natively",
      );
    }
    op_audio_disconnect(this.#nodeId);
    if (destination) {
      const idx = this.#connections.indexOf(destination);
      if (idx >= 0) this.#connections.splice(idx, 1);
    } else {
      this.#connections.length = 0;
    }
  }
}

class AudioDestinationNode extends AudioNode {
  #maxChannelCount;

  constructor(context, nodeId, maxChannelCount = 2) {
    super(context, nodeId, {
      numberOfInputs: 1,
      numberOfOutputs: 0,
    });
    this.#maxChannelCount = maxChannelCount;
  }

  get maxChannelCount() {
    return this.#maxChannelCount;
  }
}

function validateScheduledTime(value, name = "when") {
  const number = Number(value);
  if (!Number.isFinite(number) || number < 0) {
    throw new RangeError(`${name} must be a finite, non-negative number`);
  }
  return number;
}

function validateFiniteDouble(value, name) {
  const number = Number(value);
  if (!Number.isFinite(number)) {
    throw new TypeError(`${name} must be a finite number`);
  }
  return number;
}

export {
  AudioNode,
  AudioDestinationNode,
  createReleaseQueue,
  validateScheduledTime,
  validateFiniteDouble,
  warnUnsupportedCapability,
};
