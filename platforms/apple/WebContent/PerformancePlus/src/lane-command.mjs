// Ops whose work is a message to the host that nothing waits on.
//
// The embedded runtime answers these on its own thread and returns; the caller
// never sees a result. Here the host is in another process, so the message
// crosses, and the op still returns at once: a command that blocked content
// until the host had handled it would be a synchronous op in disguise.

import {
  UNUSABLE_BACKING,
  audioError,
  checkControlString,
  checkIirCoefficients,
  checkLoopPoints,
  checkOptionalDuration,
  checkPlanarBacking,
  checkScheduledTime,
  checkWaveShaperCurveBytes,
  optionalArrayBufferOf,
  transferOut,
} from "./audio.mjs";
import { engineHost } from "./engine-host.mjs";
import { servicesOf } from "./lane-async.mjs";
import { f32BitsOf, optionalBytesOf, smiU32, stringOf, toBool, toF64, toU8 } from "./op-args.mjs";
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

// ---- audio ------------------------------------------------------------------
//
// The audio graph, its parameters, and the players: commands in call order,
// applied on the host's audio thread. The embedded op refuses a malformed
// argument by throwing, so each check it makes is made here first, with its
// words (audio.mjs, held to the embedded answers by test/audio-checks.test.mjs);
// a command that passes them is sent and not answered.

function audioCommand(op, writeArgs) {
  servicesOf(engineHost()).command(op, writeArgs);
}

export function op_audio_release_context(ctxId) {
  const ctx = smiU32(ctxId, "ctx_id");
  audioCommand(SERVICE_OP.op_audio_release_context, (w) => w.u32(ctx));
}

export function op_audio_release_node(ctxId, nodeId) {
  const ctx = smiU32(ctxId, "ctx_id");
  const node = smiU32(nodeId, "node_id");
  audioCommand(SERVICE_OP.op_audio_release_node, (w) => {
    w.u32(ctx);
    w.u32(node);
  });
}

export function op_audio_abort_buffer(bufferId) {
  const id = smiU32(bufferId, "buffer_id");
  audioCommand(SERVICE_OP.op_audio_abort_buffer, (w) => w.u32(id));
}

export function op_audio_release_buffer(bufferId) {
  const id = smiU32(bufferId, "buffer_id");
  audioCommand(SERVICE_OP.op_audio_release_buffer, (w) => w.u32(id));
}

/// Publish a buffer to a source node: its host entry, and -- when content holds
/// the buffer's writable PCM -- that PCM, which the host freezes as it arrives.
/// The backing is taken from content only once every check has passed, as the
/// embedded op detaches it only once its command is accepted.
function publishBuffer(op, ctx, node, id, planar, writeTiming) {
  if (id === 0 && planar !== null) throw audioError("a null AudioBuffer id cannot carry a backing store");
  if (planar !== null) checkPlanarBacking(planar);
  const moved = planar === null ? null : transferOut(planar, UNUSABLE_BACKING);
  audioCommand(op, (w) => {
    w.u32(ctx);
    w.u32(node);
    w.u32(id);
    if (moved === null) w.null();
    else w.bytes(moved);
    writeTiming?.(w);
  });
}

export function op_audio_start_buffer(ctxId, nodeId, bufferId, backing, when, offset, duration) {
  const ctx = smiU32(ctxId, "ctx_id");
  const node = smiU32(nodeId, "node_id");
  const id = smiU32(bufferId, "buffer_id");
  const planar = optionalArrayBufferOf(backing);
  const at = toF64(when, "when");
  const from = toF64(offset, "offset");
  const length = toF64(duration, "duration");
  checkScheduledTime("when", at);
  checkScheduledTime("offset", from);
  checkOptionalDuration(length);
  publishBuffer(SERVICE_OP.op_audio_start_buffer, ctx, node, id, planar, (w) => {
    w.f64(at);
    w.f64(from);
    w.f64(length);
  });
}

export function op_audio_set_started_buffer(ctxId, nodeId, bufferId, backing) {
  const ctx = smiU32(ctxId, "ctx_id");
  const node = smiU32(nodeId, "node_id");
  const id = smiU32(bufferId, "buffer_id");
  publishBuffer(SERVICE_OP.op_audio_set_started_buffer, ctx, node, id, optionalArrayBufferOf(backing));
}

/// The nodes made from nothing but their ids.
function createNode(op, ctxId, nodeId) {
  const ctx = smiU32(ctxId, "ctx_id");
  const node = smiU32(nodeId, "node_id");
  audioCommand(op, (w) => {
    w.u32(ctx);
    w.u32(node);
  });
}

export function op_audio_create_buffer_source(ctxId, nodeId) {
  createNode(SERVICE_OP.op_audio_create_buffer_source, ctxId, nodeId);
}

export function op_audio_create_gain(ctxId, nodeId) {
  createNode(SERVICE_OP.op_audio_create_gain, ctxId, nodeId);
}

export function op_audio_create_oscillator(ctxId, nodeId) {
  createNode(SERVICE_OP.op_audio_create_oscillator, ctxId, nodeId);
}

export function op_audio_create_biquad_filter(ctxId, nodeId) {
  createNode(SERVICE_OP.op_audio_create_biquad_filter, ctxId, nodeId);
}

export function op_audio_create_wave_shaper(ctxId, nodeId) {
  createNode(SERVICE_OP.op_audio_create_wave_shaper, ctxId, nodeId);
}

export function op_audio_create_analyser(ctxId, nodeId) {
  createNode(SERVICE_OP.op_audio_create_analyser, ctxId, nodeId);
}

export function op_audio_create_dynamics_compressor(ctxId, nodeId) {
  createNode(SERVICE_OP.op_audio_create_dynamics_compressor, ctxId, nodeId);
}

export function op_audio_create_panner(ctxId, nodeId) {
  createNode(SERVICE_OP.op_audio_create_panner, ctxId, nodeId);
}

export function op_audio_create_constant_source(ctxId, nodeId) {
  createNode(SERVICE_OP.op_audio_create_constant_source, ctxId, nodeId);
}

export function op_audio_create_delay(ctxId, nodeId, maxDelayTime) {
  const ctx = smiU32(ctxId, "ctx_id");
  const node = smiU32(nodeId, "node_id");
  const max = f32BitsOf(maxDelayTime, "max_delay_time");
  audioCommand(SERVICE_OP.op_audio_create_delay, (w) => {
    w.u32(ctx);
    w.u32(node);
    w.u32(max);
  });
}

export function op_audio_create_channel_merger(ctxId, nodeId, numberOfInputs) {
  const ctx = smiU32(ctxId, "ctx_id");
  const node = smiU32(nodeId, "node_id");
  const inputs = smiU32(numberOfInputs, "number_of_inputs");
  audioCommand(SERVICE_OP.op_audio_create_channel_merger, (w) => {
    w.u32(ctx);
    w.u32(node);
    w.u32(inputs);
  });
}

export function op_audio_create_channel_splitter(ctxId, nodeId, numberOfOutputs) {
  const ctx = smiU32(ctxId, "ctx_id");
  const node = smiU32(nodeId, "node_id");
  const outputs = smiU32(numberOfOutputs, "number_of_outputs");
  audioCommand(SERVICE_OP.op_audio_create_channel_splitter, (w) => {
    w.u32(ctx);
    w.u32(node);
    w.u32(outputs);
  });
}

/// `#[serde] Vec<f64>` twice. serde has no single rule to restate here, and
/// needs none: the facade (`createIIRFilter`) hands this op arrays it has
/// already checked hold finite Numbers, and they cross as they are.
export function op_audio_create_iir_filter(ctxId, nodeId, feedforward, feedback) {
  const ctx = smiU32(ctxId, "ctx_id");
  const node = smiU32(nodeId, "node_id");
  checkIirCoefficients(feedforward, feedback);
  audioCommand(SERVICE_OP.op_audio_create_iir_filter, (w) => {
    w.u32(ctx);
    w.u32(node);
    for (const coefficients of [feedforward, feedback]) {
      w.array(coefficients.length);
      for (const coefficient of coefficients) w.f64(coefficient);
    }
  });
}

export function op_audio_set_loop(nodeId, loopEnabled, loopStart, loopEnd) {
  const node = smiU32(nodeId, "node_id");
  const enabled = toBool(loopEnabled, "loop_enabled");
  const start = toF64(loopStart, "loop_start");
  const end = toF64(loopEnd, "loop_end");
  checkLoopPoints(start, end);
  audioCommand(SERVICE_OP.op_audio_set_loop, (w) => {
    w.u32(node);
    w.bool(enabled);
    w.f64(start);
    w.f64(end);
  });
}

export function op_audio_set_gain_value(nodeId, value) {
  const node = smiU32(nodeId, "node_id");
  const gain = f32BitsOf(value, "value");
  audioCommand(SERVICE_OP.op_audio_set_gain_value, (w) => {
    w.u32(node);
    w.u32(gain);
  });
}

export function op_audio_set_node_param(nodeId, paramName, value) {
  const node = smiU32(nodeId, "node_id");
  const name = stringOf(paramName, "param_name");
  const bits = f32BitsOf(value, "value");
  checkControlString("AudioParam name", name);
  audioCommand(SERVICE_OP.op_audio_set_node_param, (w) => {
    w.u32(node);
    w.str(name);
    w.u32(bits);
  });
}

/// An AudioParam automation call: node, parameter name, then its numbers.
function automate(op, nodeId, paramName, writeNumbers) {
  const node = smiU32(nodeId, "node_id");
  const name = stringOf(paramName, "param_name");
  checkControlString("AudioParam name", name);
  audioCommand(op, (w) => {
    w.u32(node);
    w.str(name);
    writeNumbers(w);
  });
}

export function op_audio_param_set_value_at_time(nodeId, paramName, value, time) {
  const bits = f32BitsOf(value, "value");
  const at = toF64(time, "time");
  automate(SERVICE_OP.op_audio_param_set_value_at_time, nodeId, paramName, (w) => {
    w.u32(bits);
    w.f64(at);
  });
}

export function op_audio_param_linear_ramp(nodeId, paramName, value, endTime) {
  const bits = f32BitsOf(value, "value");
  const end = toF64(endTime, "end_time");
  automate(SERVICE_OP.op_audio_param_linear_ramp, nodeId, paramName, (w) => {
    w.u32(bits);
    w.f64(end);
  });
}

export function op_audio_param_exponential_ramp(nodeId, paramName, value, endTime) {
  const bits = f32BitsOf(value, "value");
  const end = toF64(endTime, "end_time");
  automate(SERVICE_OP.op_audio_param_exponential_ramp, nodeId, paramName, (w) => {
    w.u32(bits);
    w.f64(end);
  });
}

export function op_audio_param_set_target(nodeId, paramName, target, startTime, timeConstant) {
  const bits = f32BitsOf(target, "target");
  const start = toF64(startTime, "start_time");
  const constant = toF64(timeConstant, "time_constant");
  automate(SERVICE_OP.op_audio_param_set_target, nodeId, paramName, (w) => {
    w.u32(bits);
    w.f64(start);
    w.f64(constant);
  });
}

export function op_audio_param_cancel_scheduled(nodeId, paramName, cancelTime) {
  const at = toF64(cancelTime, "cancel_time");
  automate(SERVICE_OP.op_audio_param_cancel_scheduled, nodeId, paramName, (w) => w.f64(at));
}

/// A scheduled source's start or stop: node and a time on the timeline.
function schedule(op, nodeId, when) {
  const node = smiU32(nodeId, "node_id");
  const at = toF64(when, "when");
  checkScheduledTime("when", at);
  audioCommand(op, (w) => {
    w.u32(node);
    w.f64(at);
  });
}

export function op_audio_start_oscillator(nodeId, when) {
  schedule(SERVICE_OP.op_audio_start_oscillator, nodeId, when);
}

export function op_audio_stop_oscillator(nodeId, when) {
  schedule(SERVICE_OP.op_audio_stop_oscillator, nodeId, when);
}

export function op_audio_start_constant_source(nodeId, when) {
  schedule(SERVICE_OP.op_audio_start_constant_source, nodeId, when);
}

export function op_audio_stop_constant_source(nodeId, when) {
  schedule(SERVICE_OP.op_audio_stop_constant_source, nodeId, when);
}

/// A string-valued node setting, bounded like every string the queue carries.
function setting(op, field, node, value) {
  checkControlString(field, value);
  audioCommand(op, (w) => {
    w.u32(node);
    w.str(value);
  });
}

export function op_audio_set_oscillator_type(nodeId, oscType) {
  setting(
    SERVICE_OP.op_audio_set_oscillator_type,
    "oscillator type",
    smiU32(nodeId, "node_id"),
    stringOf(oscType, "osc_type"),
  );
}

export function op_audio_set_biquad_filter_type(nodeId, filterType) {
  setting(
    SERVICE_OP.op_audio_set_biquad_filter_type,
    "biquad filter type",
    smiU32(nodeId, "node_id"),
    stringOf(filterType, "filter_type"),
  );
}

export function op_audio_set_wave_shaper_oversample(nodeId, oversample) {
  setting(
    SERVICE_OP.op_audio_set_wave_shaper_oversample,
    "WaveShaper oversample",
    smiU32(nodeId, "node_id"),
    stringOf(oversample, "oversample"),
  );
}

export function op_audio_set_panning_model(nodeId, model) {
  setting(
    SERVICE_OP.op_audio_set_panning_model,
    "panning model",
    smiU32(nodeId, "node_id"),
    stringOf(model, "model"),
  );
}

export function op_audio_set_distance_model(nodeId, model) {
  setting(
    SERVICE_OP.op_audio_set_distance_model,
    "distance model",
    smiU32(nodeId, "node_id"),
    stringOf(model, "model"),
  );
}

export function op_audio_set_wave_shaper_curve(nodeId, curveBytes) {
  const node = smiU32(nodeId, "node_id");
  const curve = optionalBytesOf(curveBytes, "curve_bytes");
  checkWaveShaperCurveBytes(curve === null ? 0 : curve.byteLength);
  audioCommand(SERVICE_OP.op_audio_set_wave_shaper_curve, (w) => {
    w.u32(node);
    if (curve === null) w.null();
    else w.bytes(curve);
  });
}

export function op_audio_set_analyser_fft_size(nodeId, fftSize) {
  const node = smiU32(nodeId, "node_id");
  const size = smiU32(fftSize, "fft_size");
  audioCommand(SERVICE_OP.op_audio_set_analyser_fft_size, (w) => {
    w.u32(node);
    w.u32(size);
  });
}

export function op_audio_set_analyser_scalar(nodeId, prop, value) {
  const node = smiU32(nodeId, "node_id");
  const name = stringOf(prop, "prop");
  const bits = f32BitsOf(value, "value");
  checkControlString("analyser property", name);
  audioCommand(SERVICE_OP.op_audio_set_analyser_scalar, (w) => {
    w.u32(node);
    w.str(name);
    w.u32(bits);
  });
}

export function op_audio_set_panner_scalar(nodeId, prop, value) {
  const node = smiU32(nodeId, "node_id");
  const name = stringOf(prop, "prop");
  const number = toF64(value, "value");
  checkControlString("panner property", name);
  audioCommand(SERVICE_OP.op_audio_set_panner_scalar, (w) => {
    w.u32(node);
    w.str(name);
    w.f64(number);
  });
}

// ---- MediaAudioPlayer -----------------------------------------------------------

function player(op, playerId) {
  const id = smiU32(playerId, "player_id");
  audioCommand(op, (w) => w.u32(id));
}

function playerSource(op, playerId, sourceId) {
  const id = smiU32(playerId, "player_id");
  const source = smiU32(sourceId, "source_id");
  audioCommand(op, (w) => {
    w.u32(id);
    w.u32(source);
  });
}

export function op_media_audio_player_add_source(playerId, sourceId) {
  playerSource(SERVICE_OP.op_media_audio_player_add_source, playerId, sourceId);
}

export function op_media_audio_player_remove_source(playerId, sourceId) {
  playerSource(SERVICE_OP.op_media_audio_player_remove_source, playerId, sourceId);
}

export function op_media_audio_player_start(playerId) {
  player(SERVICE_OP.op_media_audio_player_start, playerId);
}

export function op_media_audio_player_stop(playerId) {
  player(SERVICE_OP.op_media_audio_player_stop, playerId);
}

export function op_media_audio_player_destroy(playerId) {
  player(SERVICE_OP.op_media_audio_player_destroy, playerId);
}

// ---- InnerAudioContext ------------------------------------------------------------

function inner(op, id, writeValue) {
  const target = smiU32(id, "id");
  audioCommand(op, (w) => {
    w.u32(target);
    writeValue?.(w);
  });
}

export function op_inner_audio_destroy(id) {
  inner(SERVICE_OP.op_inner_audio_destroy, id);
}

export function op_inner_audio_play(id) {
  inner(SERVICE_OP.op_inner_audio_play, id);
}

export function op_inner_audio_pause(id) {
  inner(SERVICE_OP.op_inner_audio_pause, id);
}

export function op_inner_audio_stop(id) {
  inner(SERVICE_OP.op_inner_audio_stop, id);
}

export function op_inner_audio_seek(id, position) {
  const at = toF64(position, "position");
  inner(SERVICE_OP.op_inner_audio_seek, id, (w) => w.f64(at));
}

export function op_inner_audio_set_volume(id, volume) {
  const bits = f32BitsOf(volume, "volume");
  inner(SERVICE_OP.op_inner_audio_set_volume, id, (w) => w.u32(bits));
}

export function op_inner_audio_set_loop(id, loopEnabled) {
  const enabled = toBool(loopEnabled, "loop_enabled");
  inner(SERVICE_OP.op_inner_audio_set_loop, id, (w) => w.bool(enabled));
}

export function op_inner_audio_set_playback_rate(id, rate) {
  const bits = f32BitsOf(rate, "rate");
  inner(SERVICE_OP.op_inner_audio_set_playback_rate, id, (w) => w.u32(bits));
}

export function op_inner_audio_set_autoplay(id, autoplay) {
  const enabled = toBool(autoplay, "autoplay");
  inner(SERVICE_OP.op_inner_audio_set_autoplay, id, (w) => w.bool(enabled));
}
