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
import { f32BitsOf, optionalBytesOf, optionalStringOf, smiU32, stringOf, toBool, toF64, toI32, toU8 } from "./op-args.mjs";
import { SERVICE_OP } from "./service-ops.mjs";
import { MAX_SERVICE_MESSAGE_BYTES, RECORD_HEADER_BYTES, SERVICE_HEADER_BYTES } from "./service.mjs";
import { terminateWorkerAttempt } from "./worker-relay-channel.mjs";

/// The longest console line one service record carries, in UTF-16 code units:
/// a code unit is at most three UTF-8 bytes, and the record's framing and the
/// note on a cut line fit in what is left.
const MAX_CONSOLE_UNITS = Math.floor(
  (MAX_SERVICE_MESSAGE_BYTES - SERVICE_HEADER_BYTES - RECORD_HEADER_BYTES - 256) / 3,
);

/// A console line, for the host: a command, so it reaches the host in order
/// with everything content did before and after it -- a line written before
/// `exitMiniProgram` is heard before the exit -- and the host logs it and hands
/// it to the app as the embedded op's line is (level 1 info, 2 warn, 3 error,
/// anything else debug).
///
/// The message is the value's ToString, and a value whose ToString throws is
/// logged as `<invalid value>` -- the Rust op's answer -- rather than turning a
/// log call into an exception, which would make a failing `toString` fatal to
/// whatever was being reported. For the same reason a line too long for one
/// service record is cut, and a line written after the channel broke is
/// dropped: nobody is left to hear it.
export function op_console(value, level) {
  const severity = toU8(level, "level");
  let message;
  try {
    message = `${value}`;
  } catch {
    message = "<invalid value>";
  }
  if (message.length > MAX_CONSOLE_UNITS) {
    // Not between the halves of a surrogate pair.
    const end = isLeadSurrogate(message.charCodeAt(MAX_CONSOLE_UNITS - 1))
      ? MAX_CONSOLE_UNITS - 1
      : MAX_CONSOLE_UNITS;
    message = `${message.slice(0, end)} [cut: the line was ${message.length} characters]`;
  }
  try {
    servicesOf(engineHost()).command(SERVICE_OP.op_console, (w) => {
      w.str(message);
      w.u32(severity);
    });
  } catch {
    // The channel is gone, and the host with it.
  }
}

function isLeadSurrogate(unit) {
  return unit >= 0xd800 && unit <= 0xdbff;
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

export function op_audio_watch_source_ended(nodeId, enabled) {
  const node = smiU32(nodeId, "node_id");
  const watch = toBool(enabled, "enabled");
  audioCommand(SERVICE_OP.op_audio_watch_source_ended, (w) => {
    w.u32(node);
    w.bool(watch);
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

// ---- network ------------------------------------------------------------------

/// Warm the OS resolver for the hosts content named. Nothing answers it: the
/// op returns as soon as the work is handed over in the embedded runtime too,
/// and a host the policy refuses is skipped rather than reported.
export function op_prefetch_dns(hostsJson) {
  const hosts = stringOf(hostsJson, "hosts_json");
  servicesOf(engineHost()).command(SERVICE_OP.op_prefetch_dns, (w) => w.str(hosts));
}

/// Close a TCP socket. The read in flight is cancelled on the host and the
/// connection is shut down; nothing answers, as nothing answers the embedded
/// op either.
export function op_tcp_close(rid) {
  const socket = smiU32(rid, "rid");
  servicesOf(engineHost()).command(SERVICE_OP.op_tcp_close, (w) => w.u32(socket));
}

export function op_udp_close(rid) {
  const socket = smiU32(rid, "rid");
  servicesOf(engineHost()).command(SERVICE_OP.op_udp_close, (w) => w.u32(socket));
}

export function op_udp_set_ttl(rid, ttl) {
  const socket = smiU32(rid, "rid");
  const hops = smiU32(ttl, "ttl");
  servicesOf(engineHost()).command(SERVICE_OP.op_udp_set_ttl, (w) => {
    w.u32(socket);
    w.u32(hops);
  });
}

// ---- the session, as content asks about itself -----------------------------------

/// End the session. The host stops it as it stops one the embedded runtime
/// asked to end: the same command on the same channel, so the shutdown path is
/// one path.
export function op_exit_mini_program() {
  servicesOf(engineHost()).command(SERVICE_OP.op_exit_mini_program);
}

/// Restart the session, which is how a game recovers from a state it cannot
/// leave. The generation the producer stamps its packets with changes with it,
/// which is what stops a frame built for the old one from landing in the new.
/// `launch_query` is the query the restarted game is launched with, or null for
/// the one it was last shown with.
export function op_restart_mini_program(launch_query) {
  const query = optionalStringOf(launch_query, "launch_query");
  servicesOf(engineHost()).command(SERVICE_OP.op_restart_mini_program, (w) => {
    if (query === null) w.null();
    else w.str(query);
  });
}

// ---- the soft keyboard ----------------------------------------------------------

/// Open the host's keyboard. The options stay the JSON the engine's
/// `02_keyboard.js` made, because the host reads that same JSON in process.
export function op_show_keyboard(optionsJson) {
  const options = stringOf(optionsJson, "options_json");
  servicesOf(engineHost()).command(SERVICE_OP.op_show_keyboard, (w) => w.str(options));
}

export function op_hide_keyboard() {
  servicesOf(engineHost()).command(SERVICE_OP.op_hide_keyboard);
}

/// Content correcting the field: the whole current text, not a keystroke.
export function op_update_keyboard(value) {
  const text = stringOf(value, "value");
  servicesOf(engineHost()).command(SERVICE_OP.op_update_keyboard, (w) => w.str(text));
}

// ---- the device ---------------------------------------------------------------
//
// Requests the host carries out -- a vibration, the display held awake, a log
// entry kept -- and the switch for hearing about network changes. The host
// applies each with the same device service the embedded op calls; a host
// without one logs the op's own refusal.

/// A short vibration: `"heavy"`, `"medium"` or `"light"`, as content named it.
export function op_vibrate_short(vibrateType) {
  const type = stringOf(vibrateType, "vibrate_type");
  servicesOf(engineHost()).command(SERVICE_OP.op_vibrate_short, (w) => w.str(type));
}

export function op_vibrate_long() {
  servicesOf(engineHost()).command(SERVICE_OP.op_vibrate_long);
}

export function op_set_keep_screen_on(keepOn) {
  const on = toBool(keepOn, "keep_on");
  servicesOf(engineHost()).command(SERVICE_OP.op_set_keep_screen_on, (w) => w.bool(on));
}

/// Start or stop hearing about network changes: the host sends one only while
/// content listens, as `_internalTriggerNetworkStatusChange`.
export function op_start_network_monitoring() {
  servicesOf(engineHost()).command(SERVICE_OP.op_start_network_monitoring);
}

export function op_stop_network_monitoring() {
  servicesOf(engineHost()).command(SERVICE_OP.op_stop_network_monitoring);
}

/// One game-log entry, the JSON the engine's log manager merged.
export function op_game_log_report(logJson) {
  const entry = stringOf(logJson, "log_json");
  servicesOf(engineHost()).command(SERVICE_OP.op_game_log_report, (w) => w.str(entry));
}

/// Ask for a frame rate. The host rounds it into the range the engine offers
/// and ignores one that is not a number, as the embedded op does -- the filter
/// is the host's so both executions answer the same request the same way.
export function op_set_preferred_fps(fps) {
  const rate = toF64(fps, "fps");
  servicesOf(engineHost()).command(SERVICE_OP.op_set_preferred_fps, (w) => w.f64(rate));
}

// ---- the host app's services ----------------------------------------------------
//
// What content asks of the host app -- a toast, the clipboard, a location, a
// Bluetooth connection: the host-service channel's methods
// (contracts/runtime/host-services.json). The embedded op checks the scope and
// calls one device service; the host does both here, under the op's own number,
// with what the op was given. A result reaches content through the method's hook
// as it does embedded, and so does a refusal: a command cannot throw, so the
// host settles the call it refuses through that hook rather than leave it
// waiting (core/src/runtime/service_channel.rs).
//
// One factory per parameter the ops take, so each conversion names the Rust
// parameter it restates.

function hostCommand(op) {
  return () => servicesOf(engineHost()).command(op);
}

function hostRequest(op) {
  return (requestJson) => {
    const request = stringOf(requestJson, "request_json");
    servicesOf(engineHost()).command(op, (w) => w.str(request));
  };
}

function hostOptions(op) {
  return (optionsJson) => {
    const options = stringOf(optionsJson, "options_json");
    servicesOf(engineHost()).command(op, (w) => w.str(options));
  };
}

function hostJson(op) {
  return (json) => {
    const request = stringOf(json, "json");
    servicesOf(engineHost()).command(op, (w) => w.str(request));
  };
}

function hostRequestId(op) {
  return (requestId) => {
    const id = toI32(requestId, "request_id");
    servicesOf(engineHost()).command(op, (w) => w.i32(id));
  };
}

export const op_ad_create = hostRequest(SERVICE_OP.op_ad_create);
export const op_ad_destroy = hostRequest(SERVICE_OP.op_ad_destroy);
export const op_ad_hide = hostRequest(SERVICE_OP.op_ad_hide);
export const op_ad_load = hostRequest(SERVICE_OP.op_ad_load);
export const op_ad_show = hostRequest(SERVICE_OP.op_ad_show);
export const op_ad_update_style = hostRequest(SERVICE_OP.op_ad_update_style);
export const op_apply_update = hostCommand(SERVICE_OP.op_apply_update);
export const op_ble_peripheral_add_service = hostRequest(SERVICE_OP.op_ble_peripheral_add_service);
export const op_ble_peripheral_remove_service = hostRequest(SERVICE_OP.op_ble_peripheral_remove_service);
export const op_ble_peripheral_start_advertising = hostRequest(SERVICE_OP.op_ble_peripheral_start_advertising);
export const op_ble_peripheral_stop_advertising = hostRequest(SERVICE_OP.op_ble_peripheral_stop_advertising);
export const op_ble_peripheral_write_characteristic_value = hostRequest(SERVICE_OP.op_ble_peripheral_write_characteristic_value);
export const op_authorize = hostRequest(SERVICE_OP.op_authorize);
export const op_check_session = hostOptions(SERVICE_OP.op_check_session);
export const op_choose_image = hostRequest(SERVICE_OP.op_choose_image);
export const op_choose_media = hostRequest(SERVICE_OP.op_choose_media);
export const op_choose_message_file = hostRequest(SERVICE_OP.op_choose_message_file);
export const op_close_ble_connection = hostRequest(SERVICE_OP.op_close_ble_connection);
export const op_close_bluetooth_adapter = hostRequest(SERVICE_OP.op_close_bluetooth_adapter);
export const op_create_ble_connection = hostRequest(SERVICE_OP.op_create_ble_connection);
export const op_create_ble_peripheral_server = hostRequest(SERVICE_OP.op_create_ble_peripheral_server);
export const op_ecosystem_reply = hostJson(SERVICE_OP.op_ecosystem_reply);
export const op_exit_pointer_lock = hostCommand(SERVICE_OP.op_exit_pointer_lock);
export const op_get_beacons = hostRequest(SERVICE_OP.op_get_beacons);
export const op_get_ble_device_characteristics = hostRequest(SERVICE_OP.op_get_ble_device_characteristics);
export const op_get_ble_device_rssi = hostRequest(SERVICE_OP.op_get_ble_device_rssi);
export const op_get_ble_device_services = hostRequest(SERVICE_OP.op_get_ble_device_services);
export const op_get_ble_mtu = hostRequest(SERVICE_OP.op_get_ble_mtu);
export const op_get_bluetooth_adapter_state = hostRequest(SERVICE_OP.op_get_bluetooth_adapter_state);
export const op_get_bluetooth_devices = hostRequest(SERVICE_OP.op_get_bluetooth_devices);
export const op_get_clipboard_data = hostRequest(SERVICE_OP.op_get_clipboard_data);
export const op_get_connected_bluetooth_devices = hostRequest(SERVICE_OP.op_get_connected_bluetooth_devices);
export const op_get_fuzzy_location = hostOptions(SERVICE_OP.op_get_fuzzy_location);
export const op_get_location = hostOptions(SERVICE_OP.op_get_location);
export const op_get_phone_number = hostOptions(SERVICE_OP.op_get_phone_number);
export const op_get_screen_brightness = hostRequest(SERVICE_OP.op_get_screen_brightness);
export const op_set_device_orientation = hostRequest(SERVICE_OP.op_set_device_orientation);
export const op_get_screen_recording_state = hostRequest(SERVICE_OP.op_get_screen_recording_state);
export const op_get_user_info = hostOptions(SERVICE_OP.op_get_user_info);
export const op_hide_loading = hostCommand(SERVICE_OP.op_hide_loading);
export const op_hide_toast = hostCommand(SERVICE_OP.op_hide_toast);
export const op_is_bluetooth_device_paired = hostRequest(SERVICE_OP.op_is_bluetooth_device_paired);
export const op_login = hostOptions(SERVICE_OP.op_login);
export const op_make_bluetooth_pair = hostRequest(SERVICE_OP.op_make_bluetooth_pair);
export const op_navigate_back_mini_program = hostOptions(SERVICE_OP.op_navigate_back_mini_program);
export const op_navigate_to_mini_program = hostOptions(SERVICE_OP.op_navigate_to_mini_program);
export const op_notify_ble_characteristic_value_change = hostRequest(SERVICE_OP.op_notify_ble_characteristic_value_change);
export const op_open_app_authorize_setting = hostRequestId(SERVICE_OP.op_open_app_authorize_setting);
export const op_open_bluetooth_adapter = hostRequest(SERVICE_OP.op_open_bluetooth_adapter);
export const op_open_customer_service_conversation = hostOptions(SERVICE_OP.op_open_customer_service_conversation);
export const op_open_setting = hostOptions(SERVICE_OP.op_open_setting);
export const op_open_system_bluetooth_setting = hostRequestId(SERVICE_OP.op_open_system_bluetooth_setting);
export const op_read_ble_characteristic_value = hostRequest(SERVICE_OP.op_read_ble_characteristic_value);
export const op_request_midas_payment = hostOptions(SERVICE_OP.op_request_midas_payment);
export const op_request_midas_payment_game_item = hostOptions(SERVICE_OP.op_request_midas_payment_game_item);
export const op_request_pointer_lock = hostCommand(SERVICE_OP.op_request_pointer_lock);
export const op_scan_code = hostOptions(SERVICE_OP.op_scan_code);
export const op_set_ble_mtu = hostRequest(SERVICE_OP.op_set_ble_mtu);
export const op_set_clipboard_data = hostRequest(SERVICE_OP.op_set_clipboard_data);
export const op_set_screen_brightness = hostRequest(SERVICE_OP.op_set_screen_brightness);
export const op_set_visual_effect_on_capture = hostRequest(SERVICE_OP.op_set_visual_effect_on_capture);
export const op_set_window_size = hostRequest(SERVICE_OP.op_set_window_size);
export const op_share_set_menu = hostJson(SERVICE_OP.op_share_set_menu);
export const op_show_action_sheet = hostJson(SERVICE_OP.op_show_action_sheet);
export const op_show_loading = hostJson(SERVICE_OP.op_show_loading);
export const op_show_modal = hostJson(SERVICE_OP.op_show_modal);
export const op_show_toast = hostJson(SERVICE_OP.op_show_toast);
export const op_start_accelerometer = hostRequest(SERVICE_OP.op_start_accelerometer);
export const op_start_beacon_discovery = hostRequest(SERVICE_OP.op_start_beacon_discovery);
export const op_start_bluetooth_devices_discovery = hostRequest(SERVICE_OP.op_start_bluetooth_devices_discovery);
export const op_start_capture_screen = hostCommand(SERVICE_OP.op_start_capture_screen);
export const op_start_compass = hostRequest(SERVICE_OP.op_start_compass);
export const op_start_device_motion = hostRequest(SERVICE_OP.op_start_device_motion);
export const op_start_gyroscope = hostRequest(SERVICE_OP.op_start_gyroscope);
export const op_start_screen_recording_observer = hostCommand(SERVICE_OP.op_start_screen_recording_observer);
export const op_stop_accelerometer = hostCommand(SERVICE_OP.op_stop_accelerometer);
export const op_stop_beacon_discovery = hostRequest(SERVICE_OP.op_stop_beacon_discovery);
export const op_stop_bluetooth_devices_discovery = hostRequest(SERVICE_OP.op_stop_bluetooth_devices_discovery);
export const op_stop_capture_screen = hostCommand(SERVICE_OP.op_stop_capture_screen);
export const op_stop_compass = hostCommand(SERVICE_OP.op_stop_compass);
export const op_stop_device_motion = hostCommand(SERVICE_OP.op_stop_device_motion);
export const op_stop_gyroscope = hostCommand(SERVICE_OP.op_stop_gyroscope);
export const op_stop_screen_recording_observer = hostCommand(SERVICE_OP.op_stop_screen_recording_observer);
export const op_video_destroy = hostRequest(SERVICE_OP.op_video_destroy);
export const op_video_exit_full_screen = hostRequest(SERVICE_OP.op_video_exit_full_screen);
export const op_video_pause = hostRequest(SERVICE_OP.op_video_pause);
export const op_video_play = hostRequest(SERVICE_OP.op_video_play);
export const op_video_request_full_screen = hostRequest(SERVICE_OP.op_video_request_full_screen);
export const op_video_seek = hostRequest(SERVICE_OP.op_video_seek);
export const op_video_stop = hostRequest(SERVICE_OP.op_video_stop);
export const op_write_ble_characteristic_value = hostRequest(SERVICE_OP.op_write_ble_characteristic_value);

// ---- content's migo.createWorker (the fire-and-forget half) -----------------
//
// Termination crosses the page control port. Message admission/commit are local
// producer operations in lane-local.mjs; payloads use the direct private port.
export function op_worker_terminate() {
  terminateWorkerAttempt();
}
