import { core, primordials } from "ext:core/mod.js";
const { SafeFinalizationRegistry, TypedArrayPrototypeSubarray } = primordials;

// A finalization registry to clean up underlying resources that are GC'ed.
const RESOURCE_REGISTRY = new SafeFinalizationRegistry((rid) => {
  core.tryClose(rid);
});

class ReadableStream {
  constructor(rid) {
    this._rid = rid;
    this._closed = false;
    this._pulling = false;
    RESOURCE_REGISTRY.register(this, rid, this);
  }

  tryClose() {
    this._closed = true;
    RESOURCE_REGISTRY.unregister(this);
    core.tryClose(this._rid);
  }

  // `beforeClose`, when given, runs once the body has ended and before the resource is closed: what is asked of a
  // finished resource (a request's profile) is asked while it is still there.
  async pull(buffer, onChunked, beforeClose) {
    if (this._pulling || this._closed) return;
    this._pulling = true;

    let ended = false;
    try {
      while (!this._closed) {
        const bytesRead = await core.read(this._rid, buffer);
        if (this._closed) return;
        if (bytesRead === 0) {
          if (beforeClose !== undefined) await beforeClose(this._rid);
          if (this._closed) return;
          ended = true;
          break;
        }
        await onChunked(TypedArrayPrototypeSubarray(buffer, 0, bytesRead));
      }
    } finally {
      // Both read failures and listener failures release immediately. close
      // already cancels pending reads through the service's existing flag;
      // there is no second cancelRead state machine to keep in sync.
      this.tryClose();
      this._pulling = false;
    }
    if (ended) await onChunked(undefined);
  }

  cancel() {
    this.tryClose();
  }
}

export { ReadableStream };