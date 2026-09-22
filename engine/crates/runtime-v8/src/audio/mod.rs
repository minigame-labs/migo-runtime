use deno_core::{Extension, extension};

use crate::audio::ops::{
    op_audio_abort_buffer,
    op_audio_analyser_byte_frequency,
    op_audio_analyser_byte_time_domain,
    op_audio_analyser_float_frequency,
    op_audio_analyser_float_time_domain,
    op_audio_close_context,
    op_audio_connect,
    op_audio_copy_to_channel,
    // Phase 2: AnalyserNode ops
    op_audio_create_analyser,
    // Phase 2: BiquadFilterNode ops
    op_audio_create_biquad_filter,
    // AudioBuffer data access ops
    op_audio_create_buffer,
    op_audio_create_buffer_source,
    // Phase 3: ChannelMerger/Splitter ops
    op_audio_create_channel_merger,
    op_audio_create_channel_splitter,
    // Phase 3: ConstantSourceNode ops
    op_audio_create_constant_source,
    op_audio_create_context,
    // Phase 2: DelayNode ops
    op_audio_create_delay,
    // Phase 3: DynamicsCompressorNode ops
    op_audio_create_dynamics_compressor,
    op_audio_create_gain,
    // Phase 3: IIRFilterNode ops
    op_audio_create_iir_filter,
    // Phase 2: OscillatorNode ops
    op_audio_create_oscillator,
    // Phase 3: PannerNode ops
    op_audio_create_panner,
    // Phase 2: WaveShaperNode ops
    op_audio_create_wave_shaper,
    op_audio_decode_audio_data,
    op_audio_disconnect,
    op_audio_get_available_audio_sources,
    op_audio_get_channel_data,
    // Frequency response & analysis ops
    op_audio_get_frequency_response,
    op_audio_get_reduction,
    op_audio_materialize_buffer,
    op_audio_param_cancel_scheduled,
    op_audio_param_exponential_ramp,
    op_audio_param_linear_ramp,
    op_audio_param_set_target,
    // AudioParam automation ops
    op_audio_param_set_value_at_time,
    op_audio_release_buffer,
    op_audio_release_context,
    op_audio_release_node,
    op_audio_reserve_buffer,
    // Context resume/suspend ops
    op_audio_resume_context,
    op_audio_set_analyser_fft_size,
    op_audio_set_analyser_scalar,
    op_audio_set_biquad_filter_type,
    op_audio_set_buffer,
    op_audio_set_distance_model,
    op_audio_set_gain_value,
    // Phase 4: Global audio options ops
    op_audio_set_inner_audio_option,
    op_audio_set_loop,
    op_audio_set_node_param,
    op_audio_set_oscillator_type,
    op_audio_set_panner_scalar,
    op_audio_set_panning_model,
    op_audio_set_started_buffer,
    op_audio_set_wave_shaper_curve,
    op_audio_set_wave_shaper_oversample,
    op_audio_start,
    op_audio_start_buffer,
    op_audio_start_constant_source,
    op_audio_start_oscillator,
    op_audio_stop,
    op_audio_stop_constant_source,
    op_audio_stop_oscillator,
    op_audio_suspend_context,
    // InnerAudioContext ops
    op_inner_audio_create,
    op_inner_audio_destroy,
    op_inner_audio_get_state,
    op_inner_audio_load,
    op_inner_audio_load_url,
    op_inner_audio_pause,
    op_inner_audio_play,
    op_inner_audio_seek,
    op_inner_audio_set_autoplay,
    op_inner_audio_set_loop,
    op_inner_audio_set_playback_rate,
    op_inner_audio_set_volume,
    op_inner_audio_stop,
    op_media_audio_player_add_source,
    // Phase 4: MediaAudioPlayer ops
    op_media_audio_player_create,
    op_media_audio_player_destroy,
    op_media_audio_player_remove_source,
    op_media_audio_player_start,
    op_media_audio_player_stop,
    op_recorder_pause,
    op_recorder_resume,
    // Recorder ops
    op_recorder_start,
    op_recorder_stop,
};

mod ops;

extension!(host_v8_audio,
    deps = [host_v8_base, host_v8_lifecycle],
    ops = [
        // WebAudio ops
        op_audio_create_context,
        op_audio_close_context,
        op_audio_release_context,
    op_audio_release_node,
        op_audio_resume_context,
        op_audio_suspend_context,
        op_audio_decode_audio_data,
        op_audio_release_buffer,
        op_audio_reserve_buffer,
        op_audio_abort_buffer,
        op_audio_materialize_buffer,
        op_audio_create_buffer_source,
        op_audio_set_buffer,
        op_audio_start,
        op_audio_start_buffer,
        op_audio_set_started_buffer,
        op_audio_stop,
        op_audio_set_loop,
        op_audio_create_gain,
        op_audio_set_gain_value,
        op_audio_set_node_param,
        op_audio_connect,
        op_audio_disconnect,
        // AudioParam automation ops
        op_audio_param_set_value_at_time,
        op_audio_param_linear_ramp,
        op_audio_param_exponential_ramp,
        op_audio_param_set_target,
        op_audio_param_cancel_scheduled,
        // AudioBuffer data access ops
        op_audio_create_buffer,
        op_audio_get_channel_data,
        op_audio_copy_to_channel,
        // Phase 2: OscillatorNode ops
        op_audio_create_oscillator,
        op_audio_set_oscillator_type,
        op_audio_start_oscillator,
        op_audio_stop_oscillator,
        // Phase 2: DelayNode ops
        op_audio_create_delay,
        // Phase 2: BiquadFilterNode ops
        op_audio_create_biquad_filter,
        op_audio_set_biquad_filter_type,
        // Phase 2: WaveShaperNode ops
        op_audio_create_wave_shaper,
        op_audio_set_wave_shaper_curve,
        op_audio_set_wave_shaper_oversample,
        // Phase 2: AnalyserNode ops
        op_audio_create_analyser,
        op_audio_set_analyser_fft_size,
        op_audio_analyser_byte_time_domain,
        op_audio_analyser_float_time_domain,
        // Phase 3: DynamicsCompressorNode ops
        op_audio_create_dynamics_compressor,
        // Phase 3: PannerNode ops
        op_audio_create_panner,
        op_audio_set_panning_model,
        op_audio_set_distance_model,
        op_audio_set_analyser_scalar,
    op_audio_set_panner_scalar,
        // Phase 3: ChannelMerger/Splitter ops
        op_audio_create_channel_merger,
        op_audio_create_channel_splitter,
        // Phase 3: ConstantSourceNode ops
        op_audio_create_constant_source,
        op_audio_start_constant_source,
        op_audio_stop_constant_source,
        // Phase 3: IIRFilterNode ops
        op_audio_create_iir_filter,
        // Frequency response & analysis ops
        op_audio_get_frequency_response,
        op_audio_get_reduction,
        op_audio_analyser_byte_frequency,
        op_audio_analyser_float_frequency,
        // Phase 4: Global audio options ops
        op_audio_set_inner_audio_option,
        op_audio_get_available_audio_sources,
        // Phase 4: MediaAudioPlayer ops
        op_media_audio_player_create,
        op_media_audio_player_add_source,
        op_media_audio_player_remove_source,
        op_media_audio_player_start,
        op_media_audio_player_stop,
        op_media_audio_player_destroy,
        // Recorder ops
        op_recorder_start,
        op_recorder_pause,
        op_recorder_resume,
        op_recorder_stop,
        // InnerAudioContext ops
        op_inner_audio_create,
        op_inner_audio_destroy,
        op_inner_audio_load,
        op_inner_audio_load_url,
        op_inner_audio_play,
        op_inner_audio_pause,
        op_inner_audio_stop,
        op_inner_audio_seek,
        op_inner_audio_set_volume,
        op_inner_audio_set_loop,
        op_inner_audio_set_playback_rate,
        op_inner_audio_set_autoplay,
        op_inner_audio_get_state,
    ],
    esm_entry_point = "ext:host_v8_audio/99_global_scope.js",
    esm = [
        dir "src/audio",
        "00_audio_param.js",
        "00_audio_buffer.js",
        "00_audio_node.js",
        "00_buffer_source_node.js",
        "00_gain_node.js",
        "00_oscillator_node.js",
        "00_delay_node.js",
        "00_biquad_filter_node.js",
        "00_wave_shaper_node.js",
        "00_analyser_node.js",
        "00_dynamics_compressor_node.js",
        "00_panner_node.js",
        "00_channel_merger_node.js",
        "00_channel_splitter_node.js",
        "00_constant_source_node.js",
        "00_iir_filter_node.js",
        "00_script_processor_node.js",
        "00_periodic_wave.js",
        "00_audio_listener.js",
        "01_audio_context.js",
        "02_inner_audio_context.js",
        "03_audio_interruption.js",
        "04_media_audio_player.js",
        "05_recorder_manager.js",
        "99_global_scope.js",
    ],
);

pub fn audio_extensions() -> Vec<Extension> {
    vec![host_v8_audio::init()]
}

pub fn audio_lazy_extensions() -> Vec<Extension> {
    vec![host_v8_audio::lazy_init()]
}
