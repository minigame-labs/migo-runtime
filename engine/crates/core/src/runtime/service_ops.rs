//! The numbers service ops travel under.
//!
//! `contracts/runtime/service-ops.json` is the source; this is the host's copy,
//! held to it by the test below. The producer's copy is generated from the same
//! file when the engine is staged. Append only -- see the contract.

macro_rules! service_ops {
    ($($name:ident = $id:literal,)*) => {
        /// One constant per op, named as the op is.
        #[allow(non_upper_case_globals)]
        pub(crate) mod id {
            $(pub(crate) const $name: u32 = $id;)*
        }

        /// Every op and its number, for the contract test and for naming an op
        /// in a refusal.
        pub(crate) const ALL: &[(&str, u32)] = &[$((stringify!($name), $id),)*];
    };
}

service_ops! {
    op_storage_get = 1,
    op_storage_set = 2,
    op_storage_remove = 3,
    op_storage_clear = 4,
    op_storage_info = 5,
    op_storage_get_async = 6,
    op_storage_set_async = 7,
    op_storage_remove_async = 8,
    op_storage_clear_async = 9,
    op_storage_info_async = 10,
    op_create_buffer_url = 11,
    op_revoke_buffer_url = 12,
    op_load_image = 13,
    op_load_image_subrect = 14,
    op_preload_images = 15,
    op_destroy_image = 16,
    op_clear_image_cache = 17,
    op_get_image_cache_stats = 18,
    op_access = 19,
    op_write_or_append_file = 20,
    op_open_file = 21,
    op_close_file = 22,
    op_copy_file = 23,
    op_fstat = 24,
    op_ftruncate = 25,
    op_mkdir = 26,
    op_readdir = 27,
    op_unlink = 28,
    op_rename = 29,
    op_rmdir = 30,
    op_stat = 31,
    op_write_file = 32,
    op_read_file = 33,
    op_read_fd = 34,
    op_read_fd_into = 35,
    op_read_compressed_file = 36,
    op_read_zip_entry = 37,
    op_unzip = 38,
    op_get_file_info = 39,
    op_list_saved_files = 40,
    op_access_sync = 41,
    op_write_or_append_file_sync = 42,
    op_open_file_sync = 43,
    op_close_file_sync = 44,
    op_copy_file_sync = 45,
    op_fstat_sync = 46,
    op_ftruncate_sync = 47,
    op_mkdir_sync = 48,
    op_readdir_sync = 49,
    op_unlink_sync = 50,
    op_rename_sync = 51,
    op_rmdir_sync = 52,
    op_stat_sync = 53,
    op_write_file_sync = 54,
    op_read_file_sync = 55,
    op_read_fd_sync = 56,
    op_read_fd_into_sync = 57,
    op_read_compressed_file_sync = 58,
    op_get_file_info_sync = 59,
    op_require_resolve_and_read = 60,
    op_audio_create_context = 61,
    op_audio_close_context = 62,
    op_audio_release_context = 63,
    op_audio_release_node = 64,
    op_audio_resume_context = 65,
    op_audio_suspend_context = 66,
    op_audio_decode_audio_data = 67,
    op_audio_reserve_buffer = 68,
    op_audio_abort_buffer = 69,
    op_audio_release_buffer = 70,
    op_audio_materialize_buffer = 71,
    op_audio_start_buffer = 72,
    op_audio_set_started_buffer = 73,
    op_audio_create_buffer_source = 74,
    op_audio_create_gain = 75,
    op_audio_create_oscillator = 76,
    op_audio_create_delay = 77,
    op_audio_create_biquad_filter = 78,
    op_audio_create_wave_shaper = 79,
    op_audio_create_analyser = 80,
    op_audio_create_dynamics_compressor = 81,
    op_audio_create_panner = 82,
    op_audio_create_channel_merger = 83,
    op_audio_create_channel_splitter = 84,
    op_audio_create_constant_source = 85,
    op_audio_create_iir_filter = 86,
    op_audio_stop = 87,
    op_audio_set_loop = 88,
    op_audio_set_gain_value = 89,
    op_audio_set_node_param = 90,
    op_audio_connect = 91,
    op_audio_disconnect = 92,
    op_audio_param_set_value_at_time = 93,
    op_audio_param_linear_ramp = 94,
    op_audio_param_exponential_ramp = 95,
    op_audio_param_set_target = 96,
    op_audio_param_cancel_scheduled = 97,
    op_audio_set_oscillator_type = 98,
    op_audio_start_oscillator = 99,
    op_audio_stop_oscillator = 100,
    op_audio_set_biquad_filter_type = 101,
    op_audio_set_wave_shaper_curve = 102,
    op_audio_set_wave_shaper_oversample = 103,
    op_audio_set_analyser_fft_size = 104,
    op_audio_analyser_byte_time_domain = 105,
    op_audio_analyser_float_time_domain = 106,
    op_audio_analyser_byte_frequency = 107,
    op_audio_analyser_float_frequency = 108,
    op_audio_set_panning_model = 109,
    op_audio_set_distance_model = 110,
    op_audio_set_analyser_scalar = 111,
    op_audio_set_panner_scalar = 112,
    op_audio_start_constant_source = 113,
    op_audio_stop_constant_source = 114,
    op_audio_get_frequency_response = 115,
    op_audio_get_reduction = 116,
    op_audio_set_inner_audio_option = 117,
    op_audio_get_available_audio_sources = 118,
    op_media_audio_player_create = 119,
    op_media_audio_player_add_source = 120,
    op_media_audio_player_remove_source = 121,
    op_media_audio_player_start = 122,
    op_media_audio_player_stop = 123,
    op_media_audio_player_destroy = 124,
    op_inner_audio_create = 125,
    op_inner_audio_destroy = 126,
    op_inner_audio_load_url = 127,
    op_inner_audio_play = 128,
    op_inner_audio_pause = 129,
    op_inner_audio_stop = 130,
    op_inner_audio_seek = 131,
    op_inner_audio_set_volume = 132,
    op_inner_audio_set_loop = 133,
    op_inner_audio_set_playback_rate = 134,
    op_inner_audio_set_autoplay = 135,
    op_inner_audio_get_state = 136,
    op_fetch = 137,
    op_fetch_send = 138,
    op_prefetch_dns = 139,
    // Not ops but members of deno's `core`, which this host answers because the
    // handles they name are the network service's; see `service_network`.
    core_read = 140,
    core_close = 141,
    core_try_close = 142,
    op_ws_create = 143,
    op_ws_next_event = 144,
    op_ws_send = 145,
    op_ws_close = 146,
    op_tcp_connect = 147,
    op_tcp_next_event = 148,
    op_tcp_write = 149,
    op_tcp_close = 150,
    op_udp_bind = 151,
    op_udp_connect = 152,
    op_udp_send = 153,
    op_udp_next_event = 154,
    op_udp_close = 155,
    op_udp_set_ttl = 156,
    op_fetch_upload_cancel_handle = 157,
    op_fetch_upload = 158,
    op_prefetch_assets = 159,
    op_exit_mini_program = 160,
    op_restart_mini_program = 161,
    op_set_preferred_fps = 162,
    op_get_sub_packages = 163,
    op_get_mount_generation = 164,
    op_get_subpackage_identity = 165,
    op_is_subpackage_installed = 166,
    op_is_subpackage_persisted = 167,
    op_get_workers_path = 168,
    op_show_keyboard = 169,
    op_hide_keyboard = 170,
    op_update_keyboard = 171,
}

/// The op's name, for a refusal a person will read.
pub(crate) fn name_of(op: u32) -> Option<&'static str> {
    ALL.iter().find(|(_, id)| *id == op).map(|(name, _)| *name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The host's table and the contract, entry for entry. A number that
    /// differs is a producer calling one op and the host running another with
    /// the right-looking arguments, which is why this is compared and not
    /// trusted.
    #[test]
    fn the_host_table_is_the_contract() {
        let contract: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../contracts/runtime/service-ops.json"
        ))
        .expect("the contract is JSON");
        let ops = contract["ops"].as_object().expect("an ops object");
        let mut declared: Vec<(String, u32)> = ops
            .iter()
            .map(|(name, id)| (name.clone(), id.as_u64().expect("a number") as u32))
            .collect();
        declared.sort_by_key(|(_, id)| *id);
        let mut here: Vec<(String, u32)> = ALL
            .iter()
            .map(|(name, id)| (name.to_string(), *id))
            .collect();
        here.sort_by_key(|(_, id)| *id);
        assert_eq!(here, declared);
    }

    #[test]
    fn numbers_are_unique_and_never_zero() {
        let mut seen = std::collections::HashSet::new();
        for (name, id) in ALL {
            assert_ne!(*id, 0, "{name}: zero is not an op number");
            assert!(seen.insert(*id), "{name}: {id} is used twice");
        }
    }
}
