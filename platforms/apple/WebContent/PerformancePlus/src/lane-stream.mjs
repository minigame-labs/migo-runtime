// Ops whose work rides the frame's command stream to the host.
//
// See engine-frames.mjs for the packet each frame becomes. The contract gives
// these ops no answer to send back; where the Rust op returns a status, the
// contract's `local_answer` says what the producer answers instead.

import { appendStream, endFrame, flushToHost } from "./engine-frames.mjs";

/// A facade's flushed command buffer joins the frame. Answers 0, success: the
/// producer builds the packet, and a packet the host refuses is answered on the
/// downlink as a verdict, not here.
export function op_submit_render_stream(words, usedWords) {
  appendStream(words, usedWords);
  return 0;
}

/// The frame ends; its packet is sent or held for the window.
export function op_frame_end_unified() {
  endFrame();
}

/// `gl.flush()`: what is recorded reaches the host now, and the frame goes on.
///
/// The embedded op sends the collector's pending commands to the renderer as a
/// barrier packet; this sends them as a barrier packet. Advisory in WebGL, and
/// not free -- a packet per call -- which is the embedded runtime's cost too.
export function op_gl_flush() {
  flushToHost();
}
