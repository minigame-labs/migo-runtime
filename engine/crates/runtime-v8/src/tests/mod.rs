//! Unit tests for the V8 runtime backend.
//!
//! Grouped here rather than sitting beside the modules they exercise: they were
//! seven `tests_*.rs` files at the crate root, interleaved with production
//! sources, and three of them were named after internal ticket ids whose
//! meaning lived only in the performance audit document.
//!
//! Three modules are gated on the feature that ships the API they drive. Slim
//! `cfg`-deletes whole capability extensions, so those tests would fail against a
//! `migo` namespace that correctly does not have the function -- which is what they
//! did, silently, for as long as nothing ran a Slim host suite. A gate here is a
//! statement that the profile does not ship the API; it is not a statement that
//! the test is optional.

#[cfg(feature = "api-system")]
mod ad_reward_integrity;
mod audio_aud11;
mod audio_context_close;
mod binary_helper;
mod callback_isolation;
mod canvas_follows_surface;
mod global_surface;
mod host_bridge_dispatch;
mod install_receipt;
#[cfg(feature = "api-connectivity")]
mod permission_reporting;
// Camera and recorder scopes come from `api-media`, bluetooth from
// `api-connectivity`; all four tests need both.
#[cfg(all(feature = "api-media", feature = "api-connectivity"))]
mod permission_revocation;
mod prelude;
mod published_namespace_isolation;
mod runtime_restart_boundary;
mod snapshot_fingerprint;
mod storage_isolation;
mod timers;
mod two_session_identity;
// `getUpdateManager` ships with host_v8_update, which api-system gates.
mod deferred_api;
#[cfg(feature = "api-system")]
mod update_manager_reports_no_update;
mod v8_limits;
