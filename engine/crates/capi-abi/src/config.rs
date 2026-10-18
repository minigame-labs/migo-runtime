//! Versioned engine/session configuration and capability records.

use std::{
    mem::{offset_of, size_of},
    os::raw::c_char,
};

use crate::{
    AbiStruct, MIGO_ABI_VERSION_CURRENT, MigoResult, OutputVersionPolicy, VersionedHeader,
    copy_utf8, copy_versioned,
    validate::{validate_flags, validate_reserved},
    write_versioned_output,
};

pub const MIGO_ENGINE_FLAG_ALLOW_UNSIGNED_CONTENT: u64 = 1 << 0;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MigoEngineConfig {
    pub header: VersionedHeader,
    pub flags: u64,
    pub reserved0: u32,
    pub files_dir_utf8: *const c_char,
    pub cache_dir_utf8: *const c_char,
    pub code_cache_dir_utf8: *const c_char,
    /// The Ed25519 public key content is verified against, or all zero for
    /// none.
    ///
    /// Appended: `copy_versioned` zero-extends a caller that passes the v1
    /// record, and zero is "no key", which is what that caller had.
    pub code_signing_public_key: [u8; 32],
}

// SAFETY: the raw ABI record contains only zero-valid scalar, pointer and byte
// fields. The v1 record -- everything before the appended key -- is the
// minimum accepted prefix.
unsafe impl AbiStruct for MigoEngineConfig {
    const MINIMUM_SIZE: usize = offset_of!(MigoEngineConfig, code_signing_public_key);
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedEngineConfig {
    pub allow_unsigned_content: bool,
    /// `None` when the caller supplied no key.
    pub code_signing_public_key: Option<[u8; 32]>,
    pub files_dir: String,
    pub cache_dir: String,
    pub code_cache_dir: String,
}

impl MigoEngineConfig {
    /// Copy and validate a possibly foreign caller record.
    ///
    /// # Safety
    /// `config` must be null or readable for the byte count in its header, and
    /// each non-null string must remain NUL-terminated and readable for the
    /// duration of this call.
    pub unsafe fn parse(config: *const Self) -> Result<ValidatedEngineConfig, MigoResult> {
        // SAFETY: forwarded from the public parser contract.
        let raw = unsafe { copy_versioned::<Self>(config.cast::<VersionedHeader>()) }?;
        // SAFETY: the copied pointers retain the caller's call-duration
        // validity while this method immediately copies each string.
        unsafe { raw.validate_fields() }
    }

    /// Validate a full local record and copy its borrowed strings.
    ///
    /// # Safety
    /// Each non-null string pointer must be NUL-terminated and readable for
    /// this call.
    pub unsafe fn validate(&self) -> Result<ValidatedEngineConfig, MigoResult> {
        // SAFETY: a reference to Self supplies the complete local allocation.
        let raw =
            unsafe { copy_versioned::<Self>((self as *const Self).cast::<VersionedHeader>()) }?;
        // SAFETY: forwarded from this method's string-pointer contract.
        unsafe { raw.validate_fields() }
    }

    unsafe fn validate_fields(self) -> Result<ValidatedEngineConfig, MigoResult> {
        validate_flags(self.flags, MIGO_ENGINE_FLAG_ALLOW_UNSIGNED_CONTENT)?;
        validate_reserved(self.reserved0.into())?;
        let allow_unsigned_content = self.flags & MIGO_ENGINE_FLAG_ALLOW_UNSIGNED_CONTENT != 0;
        let code_signing_public_key =
            (self.code_signing_public_key != [0; 32]).then_some(self.code_signing_public_key);
        // Both is a contradiction, not a preference: a key says "verify", the
        // flag says "do not". Refused rather than resolved either way, because
        // either resolution is a host shipping the configuration it did not mean.
        if allow_unsigned_content && code_signing_public_key.is_some() {
            return Err(crate::MIGO_ERROR_INVALID_ARGUMENT);
        }

        // SAFETY: the parser/validator contract requires each pointer to be a
        // readable NUL-terminated string for this call.
        let files_dir = unsafe { copy_utf8(self.files_dir_utf8) }?;
        let cache_dir = unsafe { copy_utf8(self.cache_dir_utf8) }?;
        let code_cache_dir = unsafe { copy_utf8(self.code_cache_dir_utf8) }?;

        Ok(ValidatedEngineConfig {
            allow_unsigned_content,
            code_signing_public_key,
            files_dir,
            cache_dir,
            code_cache_dir,
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MigoSessionConfig {
    pub header: VersionedHeader,
    pub flags: u64,
    /// The 128-bit identity an external producer's packets must carry, or all
    /// zero when there is no external producer.
    ///
    /// Appended in the second version of this record, which is why it is at the
    /// end: `copy_versioned` zero-extends a shorter caller, so a host built
    /// against v1 keeps working and gets no identity -- which is the correct
    /// answer for a host that has no producer to authenticate.
    ///
    /// Supplied by the host, not generated here. It is the shared secret
    /// between the transport and the session, and the party that owns both ends
    /// is the one that must generate it; see `include/migo/external_frames.h`.
    pub launch_nonce: [u8; 16],
    /// What network the session's content may reach, as NUL-terminated UTF-8
    /// JSON, or null for nothing at all.
    ///
    /// Appended: `copy_versioned` zero-extends a caller that passes the record
    /// without it, and null is the refusal such a caller had no way to change.
    pub network_policy_json_utf8: *const c_char,
}

// SAFETY: the repr(C) record contains only zero-valid integer and pointer
// fields. Everything before the appended network policy is the minimum
// accepted prefix.
unsafe impl AbiStruct for MigoSessionConfig {
    const MINIMUM_SIZE: usize = offset_of!(MigoSessionConfig, network_policy_json_utf8);
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedSessionConfig {
    /// `None` when the caller supplied no identity, either by passing the v1
    /// record or by leaving the field zero. An external-frame session refuses
    /// to start without one; an embedded session has no use for it.
    pub launch_nonce: Option<u128>,
    /// The network policy's JSON, copied; `None` when the caller gave none.
    /// Parsed by the library, which owns the grammar.
    pub network_policy_json: Option<String>,
}

impl MigoSessionConfig {
    /// Copy and validate a caller-owned session configuration.
    ///
    /// # Safety
    /// `config` must be null or readable for the byte count in its header, and
    /// a non-null policy pointer NUL-terminated and readable for this call.
    pub unsafe fn parse(config: *const Self) -> Result<ValidatedSessionConfig, MigoResult> {
        // SAFETY: forwarded from the public parser contract.
        let raw = unsafe { copy_versioned::<Self>(config.cast::<VersionedHeader>()) }?;
        // SAFETY: the copied pointer retains its call-duration validity.
        unsafe { raw.validate_fields() }
    }

    /// Validate a full local record and copy its borrowed string.
    ///
    /// # Safety
    /// A non-null policy pointer must be NUL-terminated and readable for this
    /// call.
    pub unsafe fn validate(&self) -> Result<ValidatedSessionConfig, MigoResult> {
        // SAFETY: a reference to Self supplies the complete local allocation.
        let raw =
            unsafe { copy_versioned::<Self>((self as *const Self).cast::<VersionedHeader>()) }?;
        // SAFETY: forwarded from this method's string-pointer contract.
        unsafe { raw.validate_fields() }
    }

    unsafe fn validate_fields(self) -> Result<ValidatedSessionConfig, MigoResult> {
        validate_flags(self.flags, 0)?;
        let network_policy_json = if self.network_policy_json_utf8.is_null() {
            None
        } else {
            // SAFETY: the caller guarantees a readable NUL-terminated string.
            Some(unsafe { copy_utf8(self.network_policy_json_utf8) }?)
        };
        Ok(ValidatedSessionConfig {
            // Read through the one conversion both records share. All-zero is
            // treated as absent rather than as a value: it is what an
            // uninitialised struct holds and what a v1 caller zero-extends to.
            launch_nonce: crate::external_frames::launch_nonce_from_bytes(self.launch_nonce),
            network_policy_json,
        })
    }
}

/// The released build of the content -- what a host that does not say loads.
pub const MIGO_CONTENT_ENVIRONMENT_RELEASE: u32 = 0;
/// A pre-release build handed to testers.
pub const MIGO_CONTENT_ENVIRONMENT_TRIAL: u32 = 1;
/// A development build.
pub const MIGO_CONTENT_ENVIRONMENT_DEVELOP: u32 = 2;

/// The longest content version a descriptor may carry, in bytes.
pub const MIGO_CONTENT_VERSION_MAX_BYTES: usize = 64;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MigoContentDescriptor {
    pub header: VersionedHeader,
    pub flags: u32,
    /// Which build of the content this is, `MIGO_CONTENT_ENVIRONMENT_*`. It was
    /// `reserved0`, which had to be zero -- and zero is the release, which is
    /// what every host that set it loaded.
    pub environment: u32,
    pub content_id_utf8: *const c_char,
    pub entry_utf8: *const c_char,
    /// The content's released version, NUL-terminated UTF-8, or null for none.
    ///
    /// Appended: `copy_versioned` zero-extends a caller that passes the v1
    /// record, and null is "no version", which is what that caller said.
    pub version_utf8: *const c_char,
}

// SAFETY: the repr(C) record contains only zero-valid scalar and pointer
// fields. The v1 record -- everything before the appended version -- is the
// minimum accepted prefix.
unsafe impl AbiStruct for MigoContentDescriptor {
    const MINIMUM_SIZE: usize = offset_of!(MigoContentDescriptor, version_utf8);
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedContentDescriptor {
    pub content_id: String,
    pub entry: String,
    /// `MIGO_CONTENT_ENVIRONMENT_*`, checked.
    pub environment: u32,
    /// `None` when the host gave no version.
    pub version: Option<String>,
}

impl MigoContentDescriptor {
    /// Copy and validate a caller-owned content descriptor.
    ///
    /// # Safety
    /// `content` must be null or readable for its announced byte count, and
    /// both string pointers must remain valid through this call.
    pub unsafe fn parse(content: *const Self) -> Result<ValidatedContentDescriptor, MigoResult> {
        // SAFETY: forwarded from the public parser contract.
        let raw = unsafe { copy_versioned::<Self>(content.cast::<VersionedHeader>()) }?;
        // SAFETY: the copied pointers retain their call-duration validity.
        unsafe { raw.validate_fields() }
    }

    /// Validate a full local record and copy its borrowed strings.
    ///
    /// # Safety
    /// Every non-null string pointer must be NUL-terminated and readable for
    /// this call.
    pub unsafe fn validate(&self) -> Result<ValidatedContentDescriptor, MigoResult> {
        // SAFETY: a reference to Self supplies the complete local allocation.
        let raw =
            unsafe { copy_versioned::<Self>((self as *const Self).cast::<VersionedHeader>()) }?;
        // SAFETY: forwarded from this method's string-pointer contract.
        unsafe { raw.validate_fields() }
    }

    unsafe fn validate_fields(self) -> Result<ValidatedContentDescriptor, MigoResult> {
        validate_flags(self.flags.into(), 0)?;
        if self.environment > MIGO_CONTENT_ENVIRONMENT_DEVELOP {
            return Err(crate::MIGO_ERROR_INVALID_ARGUMENT);
        }

        // SAFETY: the parser/validator contract requires readable C strings.
        let content_id = unsafe { copy_utf8(self.content_id_utf8) }?;
        let entry = unsafe { copy_utf8(self.entry_utf8) }?;
        let version = if self.version_utf8.is_null() {
            None
        } else {
            // SAFETY: as above; null was handled.
            let version = unsafe { copy_utf8(self.version_utf8) }?;
            if version.len() > MIGO_CONTENT_VERSION_MAX_BYTES {
                return Err(crate::MIGO_ERROR_INVALID_ARGUMENT);
            }
            Some(version).filter(|version| !version.is_empty())
        };
        Ok(ValidatedContentDescriptor {
            content_id,
            entry,
            environment: self.environment,
            version,
        })
    }
}

const _: () = assert!(offset_of!(MigoEngineConfig, header) == 0);
const _: () = assert!(offset_of!(MigoEngineConfig, flags) == 8);
const _: () = assert!(offset_of!(MigoEngineConfig, reserved0) == 16);
const _: () = assert!(offset_of!(MigoSessionConfig, header) == 0);
const _: () = assert!(offset_of!(MigoSessionConfig, flags) == 8);
const _: () = assert!(offset_of!(MigoSessionConfig, launch_nonce) == 16);
const _: () = assert!(<MigoSessionConfig as AbiStruct>::MINIMUM_SIZE == 32);
const _: () = assert!(offset_of!(MigoSessionConfig, flags) == 8);
const _: () = assert!(offset_of!(MigoContentDescriptor, header) == 0);
const _: () = assert!(offset_of!(MigoContentDescriptor, flags) == 8);
const _: () = assert!(offset_of!(MigoContentDescriptor, environment) == 12);

#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<MigoSessionConfig>() == 40);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoSessionConfig, network_policy_json_utf8) == 32);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<MigoEngineConfig>() == 80);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoEngineConfig, code_signing_public_key) == 48);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoEngineConfig, files_dir_utf8) == 24);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoEngineConfig, cache_dir_utf8) == 32);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoEngineConfig, code_cache_dir_utf8) == 40);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<MigoContentDescriptor>() == 40);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoContentDescriptor, version_utf8) == 32);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoContentDescriptor, content_id_utf8) == 16);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(offset_of!(MigoContentDescriptor, entry_utf8) == 24);

/// Stable v1 output prefix returned by `migo_query_capabilities`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MigoCapabilities {
    pub header: VersionedHeader,
    pub abi_version_min: u32,
    pub abi_version_max: u32,
    pub platform_kinds: u64,
}

// The first released output prefix is immutable. These assertions live beside
// the authoritative Rust record so drift fails even when the runtime is not
// compiled or linked on the host.
const _: () = assert!(size_of::<MigoCapabilities>() == 24);
const _: () = assert!(offset_of!(MigoCapabilities, header) == 0);
const _: () = assert!(offset_of!(MigoCapabilities, abi_version_min) == 8);
const _: () = assert!(offset_of!(MigoCapabilities, abi_version_max) == 12);
const _: () = assert!(offset_of!(MigoCapabilities, platform_kinds) == 16);

// SAFETY: this is a repr(C) aggregate of integers and VersionedHeader; zero is
// valid for every field, and its complete 24-byte v1 prefix is required.
unsafe impl AbiStruct for MigoCapabilities {}

/// Write the capability answer into the caller's negotiated prefix.
///
/// # Safety
/// `out` must be null or aligned for [`MigoCapabilities`], with a readable
/// eight-byte header and writable storage for the announced `struct_size`.
pub unsafe fn write_capabilities(
    out: *mut u8,
    abi_version_min: u32,
    abi_version_max: u32,
    platform_kinds: u64,
) -> MigoResult {
    let value = MigoCapabilities {
        header: VersionedHeader {
            struct_size: size_of::<MigoCapabilities>() as u32,
            abi_version: MIGO_ABI_VERSION_CURRENT,
        },
        abi_version_min,
        abi_version_max,
        platform_kinds,
    };

    // SAFETY: the caller contract is forwarded unchanged to the bounded writer
    // and `value` is a distinct local record.
    unsafe {
        write_versioned_output(
            out.cast::<MigoCapabilities>(),
            &value,
            OutputVersionPolicy::CapabilityNegotiation,
        )
    }
}
