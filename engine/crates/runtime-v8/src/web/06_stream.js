import { core, internals, primordials } from "ext:core/mod.js";
const { SafeFinalizationRegistry, ArrayBuffer, TypedArrayPrototypeGetByteLength } = primordials;

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

  async readAll() {
    const chunk = await core.readAll(this._rid);
    if (TypedArrayPrototypeGetByteLength(chunk) > 0) {
      return chunk;
    }
    this.tryClose();
    return undefined;
  }

  // `beforeClose`, when given, runs once the body has ended and before the resource is closed: what is asked of a
  // finished resource (a request's profile) is asked while it is still there.
  async pull(buffer, onChunked, beforeClose) {
    if (this._pulling || this._closed) return;
    this._pulling = true;

    let bytesRead = await core.read(this._rid, buffer);
    while (bytesRead > 0) {
      if (this._closed) {
        return;
      }
      await onChunked(buffer.subarray(0, bytesRead));
      bytesRead = await core.read(this._rid, buffer);
    }
    if (beforeClose !== undefined) await beforeClose(this._rid);
    this.tryClose();
    await onChunked(undefined);
  }

  cancel() {
    this.tryClose();
  }
}

export { ReadableStream };