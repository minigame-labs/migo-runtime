// The exception the specification names for a rejected argument or a call in the wrong state.
//
// The engine's own modules throw it from paths that must work with no adapter installed:
// `DOMException` is the host page's to provide, not this runtime's, and `new DOMException(...)`
// in a module that runs without one is a ReferenceError, which is what an audio `createBuffer`
// with a bad sample rate, `createImageData(0, 1)` and a second `start()` on an oscillator threw
// instead of the `NotSupportedError`, `IndexSizeError` and `InvalidStateError` the specification
// names. Content that catches by `name` or `code` saw a different error class entirely.
//
// So: when an adapter has installed `DOMException`, its class is used (so `instanceof DOMException`
// in content holds); otherwise this one, which carries the same `name`, `message` and legacy
// numeric `code`. The global is looked up when the error is made, not when the module loads,
// because an adapter may install it after this file was evaluated. This module installs nothing
// on the global object: the engine's capability surface is `migo.*`.

const LEGACY_CODES = {
  IndexSizeError: 1,
  HierarchyRequestError: 3,
  WrongDocumentError: 4,
  InvalidCharacterError: 5,
  NoModificationAllowedError: 7,
  NotFoundError: 8,
  NotSupportedError: 9,
  InUseAttributeError: 10,
  InvalidStateError: 11,
  SyntaxError: 12,
  InvalidModificationError: 13,
  NamespaceError: 14,
  InvalidAccessError: 15,
  TypeMismatchError: 17,
  SecurityError: 18,
  NetworkError: 19,
  AbortError: 20,
  URLMismatchError: 21,
  QuotaExceededError: 22,
  TimeoutError: 23,
  InvalidNodeTypeError: 24,
  DataCloneError: 25,
};

class EngineDOMException extends Error {
  constructor(message = "", name = "Error") {
    super(message);
    Object.defineProperty(this, "name", { value: name, configurable: true, writable: true });
    Object.defineProperty(this, "code", {
      value: LEGACY_CODES[name] || 0,
      configurable: true,
      writable: true,
    });
  }
}

/** `new DOMException(message, name)` of the adapter's class if there is one, else the engine's. */
function domException(message, name) {
  const Installed = globalThis.DOMException;
  return typeof Installed === "function"
    ? new Installed(message, name)
    : new EngineDOMException(message, name);
}

export { domException, EngineDOMException };
