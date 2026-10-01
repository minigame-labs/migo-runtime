// Global scope registration for host_v8_audio APIs (api-media feature gate).

import * as audio from 'ext:host_v8_audio/01_audio_context.js';
import * as innerAudio from 'ext:host_v8_audio/02_inner_audio_context.js';
import * as audioInterruption from 'ext:host_v8_audio/03_audio_interruption.js';
import * as mediaAudioPlayer from 'ext:host_v8_audio/04_media_audio_player.js';
import * as sourceEnded from 'ext:host_v8_audio/00_source_ended.js';
import * as recorderManager from 'ext:host_v8_audio/05_recorder_manager.js';

import { primordials, core } from "ext:core/mod.js";
const { ObjectDefineProperties } = primordials;

ObjectDefineProperties(globalThis, {
    // Audio (WebAudio API)
    AudioContext: core.propNonEnumerable(audio.AudioContext),
    AudioBuffer: core.propNonEnumerable(audio.AudioBuffer),
    AudioNode: core.propNonEnumerable(audio.AudioNode),
    AudioDestinationNode: core.propNonEnumerable(audio.AudioDestinationNode),
    AudioBufferSourceNode: core.propNonEnumerable(audio.AudioBufferSourceNode),
    GainNode: core.propNonEnumerable(audio.GainNode),
    AudioParam: core.propNonEnumerable(audio.AudioParam),
    OscillatorNode: core.propNonEnumerable(audio.OscillatorNode),
    DelayNode: core.propNonEnumerable(audio.DelayNode),
    BiquadFilterNode: core.propNonEnumerable(audio.BiquadFilterNode),
    WaveShaperNode: core.propNonEnumerable(audio.WaveShaperNode),
    AnalyserNode: core.propNonEnumerable(audio.AnalyserNode),
    DynamicsCompressorNode: core.propNonEnumerable(audio.DynamicsCompressorNode),
    PannerNode: core.propNonEnumerable(audio.PannerNode),
    ChannelMergerNode: core.propNonEnumerable(audio.ChannelMergerNode),
    ChannelSplitterNode: core.propNonEnumerable(audio.ChannelSplitterNode),
    ConstantSourceNode: core.propNonEnumerable(audio.ConstantSourceNode),
    IIRFilterNode: core.propNonEnumerable(audio.IIRFilterNode),
    ScriptProcessorNode: core.propNonEnumerable(audio.ScriptProcessorNode),
    PeriodicWave: core.propNonEnumerable(audio.PeriodicWave),
    AudioListener: core.propNonEnumerable(audio.AudioListener),
    createWebAudioContext: core.propNonEnumerable(audio.createWebAudioContext),

    // InnerAudioContext
    InnerAudioContext: core.propNonEnumerable(innerAudio.InnerAudioContext),
    createInnerAudioContext: core.propNonEnumerable(innerAudio.createInnerAudioContext),
    setInnerAudioOption: core.propNonEnumerable(innerAudio.setInnerAudioOption),
    getAvailableAudioSources: core.propNonEnumerable(innerAudio.getAvailableAudioSources),
    _internalEnqueueInnerAudioEvent: core.propNonEnumerable(innerAudio._internalEnqueueInnerAudioEvent),
    _internalTriggerAudioSourceEnded: core.propNonEnumerable(sourceEnded._internalTriggerAudioSourceEnded),

    // MediaAudioPlayer
    MediaAudioPlayer: core.propNonEnumerable(mediaAudioPlayer.MediaAudioPlayer),
    createMediaAudioPlayer: core.propNonEnumerable(mediaAudioPlayer.createMediaAudioPlayer),

    // RecorderManager
    getRecorderManager: core.propNonEnumerable(recorderManager.getRecorderManager),
    _internalOnRecorderEvent: core.propNonEnumerable(recorderManager._internalOnRecorderEvent),
    _internalOnRecorderFrameData: core.propNonEnumerable(recorderManager._internalOnRecorderFrameData),

    // Audio Interruption
    onAudioInterruptionBegin: core.propNonEnumerable(audioInterruption.onAudioInterruptionBegin),
    offAudioInterruptionBegin: core.propNonEnumerable(audioInterruption.offAudioInterruptionBegin),
    onAudioInterruptionEnd: core.propNonEnumerable(audioInterruption.onAudioInterruptionEnd),
    offAudioInterruptionEnd: core.propNonEnumerable(audioInterruption.offAudioInterruptionEnd),
    _internalTriggerAudioInterruptionBegin: core.propNonEnumerable(audioInterruption._internalTriggerAudioInterruptionBegin),
    _internalTriggerAudioInterruptionEnd: core.propNonEnumerable(audioInterruption._internalTriggerAudioInterruptionEnd),
});
