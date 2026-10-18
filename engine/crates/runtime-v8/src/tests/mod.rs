//! Unit tests for the V8 runtime backend.
//!
//! Grouped here rather than sitting beside the modules they exercise: they were
//! seven `tests_*.rs` files at the crate root, interleaved with production
//! sources, and three of them were named after internal ticket ids whose
//! meaning lived only in the performance audit document.
//!
//! A module is gated on the feature that ships the API it drives. Slim
//! `cfg`-deletes whole capability extensions, so those tests would fail against a
//! `migo` namespace that correctly does not have the function -- which is what they
//! did, silently, for as long as nothing ran a Slim host suite. A gate here is a
//! statement that the profile does not ship the API; it is not a statement that
//! the test is optional.

#[cfg(feature = "api-system")]
mod ad_reward_integrity;
#[cfg(feature = "api-system")]
mod ad_splash_status;
mod api_failures;
mod api_reach;
#[cfg(feature = "api-media")]
mod audio_aud11;
#[cfg(feature = "api-media")]
mod audio_context_close;
#[cfg(feature = "api-media")]
mod audio_source_ended;
mod binary_helper;
mod callback_isolation;
mod canvas_follows_surface;
mod canvas_temp_file;
mod code_cache_dir;
#[cfg(feature = "api-sensors")]
mod device_motion;
mod fetch_through_the_service;
mod gamepad_haptics;
mod global_surface;
mod host_bridge_dispatch;
mod host_events_contract;
#[cfg(feature = "api-media")]
mod inner_audio_canplay;
#[cfg(feature = "api-media")]
mod inner_audio_error_codes;
mod input_clock;
mod install_receipt;
mod intl;
#[cfg(feature = "api-sensors")]
mod location_coordinates;
#[cfg(feature = "api-system")]
mod native_buttons;
mod op_args_agreement;
#[cfg(feature = "api-connectivity")]
mod permission_reporting;
#[cfg(feature = "api-connectivity")]
mod window_resize;
// Camera and recorder scopes come from `api-media`, bluetooth from
// `api-connectivity`; all four tests need both.
#[cfg(all(feature = "api-media", feature = "api-connectivity"))]
mod permission_revocation;
mod prelude;
mod published_namespace_isolation;
#[cfg(feature = "api-connectivity")]
mod reporting;
mod request_through_the_engine;
mod runtime_restart_boundary;
mod save_file_to_disk;
#[cfg(feature = "api-commerce")]
mod share;
mod snapshot_fingerprint;
mod storage_isolation;
mod support;
mod tcp_socket_rules;
mod timers;
mod two_session_identity;
// `getUpdateManager` ships with host_v8_update, which api-system gates.
#[cfg(feature = "api-connectivity")]
mod app_info;
#[cfg(feature = "api-sensors")]
mod debug_aids;
mod deferred_api;
mod entry_evaluation;
#[cfg(feature = "api-connectivity")]
mod ecosystem;
#[cfg(feature = "api-system")]
mod update_manager;
mod v8_limits;
#[cfg(feature = "api-media")]
mod video_decoder;
#[cfg(feature = "api-media")]
mod video_paths;
mod window_service;
