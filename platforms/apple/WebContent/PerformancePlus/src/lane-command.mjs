// Ops whose work is a message to the host that nothing waits on.
//
// The embedded runtime answers these on its own thread and returns; the caller
// never sees a result. Here the host is in another process, so the message
// crosses, and the op still returns at once: a command that blocked content
// until the host had handled it would be a synchronous op in disguise.

import { engineHost } from "./engine-host.mjs";
import { servicesOf } from "./lane-async.mjs";
import { smiU32, toU8 } from "./op-args.mjs";
import { SERVICE_OP } from "./service-ops.mjs";

/// A console line, for the host's log.
///
/// The embedded op writes it to the engine's log at the level given (1 info,
/// 2 warn, 3 error, anything else debug) and nothing else; the host maps the
/// same numbers onto its platform log. The message is the value's ToString,
/// and a value whose ToString throws is logged as `<invalid value>` -- the Rust
/// op's answer -- rather than turning a log call into an exception, which
/// would make a failing `toString` fatal to whatever was being reported.
export function op_console(value, level) {
  const severity = toU8(level, "level");
  let message;
  try {
    message = `${value}`;
  } catch {
    message = "<invalid value>";
  }
  engineHost().report({ type: "console", level: severity, message });
}

// ---- images ------------------------------------------------------------------

/// Release an image's claim on its texture. Answers `true`, as the contract's
/// `local_answer` says: the id is the producer's, and the host's table is what
/// the command updates.
export function op_destroy_image(imageId) {
  servicesOf(engineHost()).command(SERVICE_OP.op_destroy_image, (w) => w.u32(smiU32(imageId, "image_id")));
  return true;
}

/// Clear this game's image caches: its textures, its decoded bytes, its derived
/// disk cache.
export function op_clear_image_cache() {
  servicesOf(engineHost()).command(SERVICE_OP.op_clear_image_cache);
}
