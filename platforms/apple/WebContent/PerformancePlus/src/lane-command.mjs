// Ops whose work is a message to the host that nothing waits on.
//
// The embedded runtime answers these on its own thread and returns; the caller
// never sees a result. Here the host is in another process, so the message
// crosses, and the op still returns at once: a command that blocked content
// until the host had handled it would be a synchronous op in disguise.

import { engineHost } from "./engine-host.mjs";

/// A console line, for the host's log.
///
/// The embedded op writes it to the engine's log at the level given (1 info,
/// 2 warn, 3 error, anything else debug) and nothing else; the host maps the
/// same numbers onto its platform log. The message is the value's ToString,
/// and a value whose ToString throws is logged as `<invalid value>` -- the Rust
/// op's answer -- rather than turning a log call into an exception, which
/// would make a failing `toString` fatal to whatever was being reported.
export function op_console(value, level) {
  let message;
  try {
    message = `${value}`;
  } catch {
    message = "<invalid value>";
  }
  engineHost().report({ type: "console", level: level & 0xff, message });
}
