// Ops whose work rides the frame's command stream to the host.
//
// See engine-frames.mjs for the packet each frame becomes. The contract gives
// these ops no answer to send back; where the Rust op returns a status, the
// contract's `local_answer` says what the producer answers instead.

import { appendStream, endFrame } from "./engine-frames.mjs";

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
