//! Build-time identity and verification for Migo release artifacts.

use std::{
    collections::{BTreeMap, HashSet},
    fmt, fs,
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const SLICE_SCHEMA_V1: &str = "migo-artifact-manifest/v1";
pub const V8_COMPONENT_SCHEMA_V1: &str = "migo-v8-component-manifest/v1";
pub const PACKAGE_INDEX_SCHEMA_V1: &str = "migo-artifact-package-index/v1";
pub const RELEASE_ATTESTATION_SCHEMA_V1: &str = "migo-release-attestation/v1";
pub const LINUX_PACKAGE_SCHEMA_V2: &str = "migo-linux-package-manifest/v2";

/// Which C runtime the Windows V8 is compiled against. Unlike the Linux floors,
/// this is not a version the loader enforces -- the MSVC runtime is a
/// redistributable the host ships -- so what matters is that every artifact in
/// one binary agrees on it. Mixing /MD and /MT is what produced the LNK4098
/// libcmt conflict this build recipe exists to avoid.
pub const WINDOWS_MSVC_RUNTIME: &str = "MD (dynamic CRT)";

/// Loader ABI floors for the Linux GNU slice.
///
/// These are policy, not measurement: the build is pinned to a Debian bullseye
/// sysroot and the SDK contract audits the shipped binaries for any `GLIBC_*`
/// or `GLIBCXX_*` requirement above them. Measured values today sit below both
/// (2.27 / 3.4.26), and the headroom is deliberate -- the floor is what is
/// promised to consumers, so it may only be raised as a breaking change.
pub const LINUX_GLIBC_FLOOR: &str = "2.31";
pub const LINUX_GLIBCXX_FLOOR: &str = "3.4.28";

pub const ANDROID_PACKAGE_SCHEMA_V2: &str = "migo-android-package-manifest/v2";

/// The project's minimum Android API. Pinned as policy: raising it is a support
/// contract change, so a manifest declaring anything else is rejected rather
/// than silently accepted.
pub const ANDROID_API_FLOOR: &str = "26";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SliceManifest {
    pub schema: String,
    pub artifact_id: String,
    pub product_profile: String,
    pub build_type: String,
    pub codegen_profile: String,
    pub target: TargetIdentity,
    pub toolchain: ToolchainIdentity,
    pub runtime: RuntimeIdentity,
    pub snapshots: Vec<SnapshotIdentity>,
    pub graphics: GraphicsIdentity,
    pub hashes: ArtifactHashes,
    pub provenance: ProvenanceIdentity,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TargetIdentity {
    pub triple: String,
    pub os: String,
    pub arch: String,
    pub abi: String,
    pub cpu_baseline: String,
    pub required_cpu_features: Vec<String>,
    pub runtime_floor: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolchainIdentity {
    pub rustc: String,
    pub compiler: String,
    pub sdk: String,
    pub linker: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeIdentity {
    pub backend: String,
    pub rusty_v8_version: Option<String>,
    pub rusty_v8_revision: Option<String>,
    pub v8_revision: Option<String>,
    pub normalized_gn_args: Vec<String>,
    pub patches: Vec<PatchIdentity>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PatchIdentity {
    pub id: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotIdentity {
    pub runtime_kind: String,
    pub product_profile: String,
    pub target_triple: String,
    pub arch: String,
    pub schema: String,
    pub generator: String,
    pub generation_cpu_policy: String,
    pub normalized_parameters: Vec<String>,
    pub external_references_hash: String,
    pub bootstrap_inputs_hash: String,
    pub bytes_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GraphicsIdentity {
    pub backend_family: String,
    pub required_api: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactHashes {
    pub runtime_binary: String,
    pub v8_archive: Option<String>,
    pub rust_binding: Option<String>,
    pub cxx_runtime: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProvenanceIdentity {
    pub source_revision: String,
    pub build_recipe: String,
    pub build_recipe_sha256: String,
    pub licenses: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct V8ComponentManifest {
    pub schema: String,
    pub component_id: String,
    pub target: TargetIdentity,
    pub toolchain: ToolchainIdentity,
    pub runtime: RuntimeIdentity,
    pub hashes: V8ComponentHashes,
    pub provenance: ProvenanceIdentity,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct V8ComponentHashes {
    pub archive: String,
    pub rust_binding: String,
}

/// One shipped Linux GNU slice.
///
/// Kept separate from [`SliceManifest`] rather than folded into it. The Android
/// slice pins an `android_api` floor and always carries a snapshot; the Linux
/// one pins glibc and GLIBCXX floors and currently carries none. Widening the
/// Android type to accommodate both would have made every one of those fields
/// optional, which is precisely how a missing floor becomes unnoticeable.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxPackageManifest {
    pub schema: String,
    pub version: String,
    pub product_profile: String,
    pub build_type: String,
    pub codegen_profile: String,
    pub target: String,
    pub os: String,
    pub abi: String,
    pub arch: String,
    pub cpu_baseline: String,
    pub required_cpu_features: Vec<String>,
    pub glibc_floor: String,
    pub glibcxx_floor: String,
    pub sysroot: String,
    pub dynamic_dependencies: Vec<String>,
    pub snapshot_policy: String,
    pub snapshots: Vec<PackageSnapshotIdentity>,
    pub v8: V8ComponentManifest,
    pub toolchain: ToolchainIdentity,
    pub graphics: GraphicsIdentity,
    pub provenance: ProvenanceIdentity,
    pub artifacts: BTreeMap<String, PackageArtifactIdentity>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageSnapshotIdentity {
    pub runtime_kind: String,
    pub product_profile: String,
    pub target_triple: String,
    pub arch: String,
    pub schema: String,
    pub generator: String,
    pub generation_cpu_policy: String,
    pub normalized_parameters: Vec<String>,
    pub external_references_hash: String,
    pub bootstrap_inputs_hash: String,
    pub features: Vec<String>,
    pub features_hash: String,
    pub rust_sources_hash: String,
    pub v8_archive_hash: String,
    pub bytes_size: u64,
    pub bytes_hash: String,
    pub js_sources_hash: String,
    pub deno_core_version: String,
}

/// Identity of one regular file shipped in a staged SDK package.
///
/// The map is the complete regular-file set except for the manifest itself;
/// the verifier rejects undeclared extras and platform-invalid symlinks. The
/// key is package-relative. Size catches truncation cheaply; SHA-256 binds the
/// manifest to actual bytes rather than a plausible file name.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageArtifactIdentity {
    pub size_bytes: u64,
    pub sha256: String,
}

/// One shipped Android C ABI package slice, one Android ABI.
///
/// Kept separate from [`LinuxPackageManifest`] for the same reason that is kept
/// separate from the AAR [`SliceManifest`]: the fields that must be present are
/// different, and making them optional to share a type is how a missing floor or
/// a missing snapshot stops being noticed. Android is cross-compiled, ships a
/// static library rather than a versioned shared object, pins an `android_api`
/// floor rather than glibc, and always embeds a snapshot -- where Linux embeds
/// none.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AndroidPackageManifest {
    pub schema: String,
    pub version: String,
    pub product_profile: String,
    pub build_type: String,
    pub codegen_profile: String,
    pub os: String,
    pub abi: String,
    pub arch: String,
    pub android_abi: String,
    pub target_triple: String,
    pub cpu_baseline: String,
    pub required_cpu_features: Vec<String>,
    pub min_android_api: String,
    /// The `-l` flags the consumer must add when it links the static library
    /// into its own `.so`. The Android analogue of the Linux package's
    /// `dynamic_dependencies`: a static archive carries no DT_NEEDED of its own,
    /// so the transitive system libraries are the consumer's to provide.
    pub link_libraries: Vec<String>,
    pub snapshot_policy: String,
    pub snapshots: Vec<PackageSnapshotIdentity>,
    pub v8: V8ComponentManifest,
    pub toolchain: ToolchainIdentity,
    pub graphics: GraphicsIdentity,
    pub provenance: ProvenanceIdentity,
    pub artifacts: BTreeMap<String, PackageArtifactIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SliceManifestSource {
    pub package_path: String,
    pub file_path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageIndex {
    pub schema: String,
    pub product_profile: String,
    pub build_type: String,
    pub codegen_profile: String,
    pub slices: Vec<SliceIndexEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SliceIndexEntry {
    pub target_triple: String,
    pub arch: String,
    pub manifest_path: String,
    pub manifest_sha256: String,
    pub artifact_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseAttestation {
    pub schema: String,
    pub package_file: String,
    pub package_size_bytes: u64,
    pub package_sha256: String,
    pub package_index_file: String,
    pub package_index_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestError(String);

impl ManifestError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ManifestError {}

pub fn seal_slice_manifest(manifest: &mut SliceManifest) -> Result<(), ManifestError> {
    manifest.artifact_id = recompute_artifact_id(manifest)?;
    Ok(())
}

pub fn recompute_artifact_id(manifest: &SliceManifest) -> Result<String, ManifestError> {
    Ok(sha256_bytes(&canonical_identity_json(manifest)?))
}

pub fn canonical_identity_json(manifest: &SliceManifest) -> Result<Vec<u8>, ManifestError> {
    canonical_json_without_field(manifest, "artifact_id", "slice manifest")
}

pub fn validate_slice_manifest(manifest: &SliceManifest) -> Result<(), ManifestError> {
    require_equal("schema", &manifest.schema, SLICE_SCHEMA_V1)?;
    require_one_of(
        "product_profile",
        &manifest.product_profile,
        &["full", "slim"],
    )?;
    require_one_of("build_type", &manifest.build_type, &["debug", "release"])?;
    require_one_of(
        "codegen_profile",
        &manifest.codegen_profile,
        &["z", "2", "3"],
    )?;
    if manifest.build_type == "debug" && manifest.codegen_profile != "z" {
        return Err(ManifestError::new(
            "codegen_profile 2/3 requires a release build",
        ));
    }

    validate_android_target(&manifest.target)?;
    validate_toolchain(&manifest.toolchain)?;
    validate_v8_runtime(&manifest.runtime)?;
    validate_snapshots(
        &manifest.snapshots,
        &manifest.product_profile,
        &manifest.target,
    )?;
    require_non_placeholder("graphics.backend_family", &manifest.graphics.backend_family)?;
    require_non_placeholder("graphics.required_api", &manifest.graphics.required_api)?;
    validate_hashes(&manifest.hashes)?;
    validate_migo_provenance(&manifest.provenance)?;

    require_sha256("artifact_id", &manifest.artifact_id)?;
    let expected = recompute_artifact_id(manifest)?;
    if manifest.artifact_id != expected {
        return Err(ManifestError::new(format!(
            "artifact_id mismatch (manifest={}, computed={expected})",
            manifest.artifact_id
        )));
    }
    Ok(())
}

/// Validate one Linux GNU package manifest.
///
/// The rule this exists to enforce is that a library and the things built into
/// it must describe the same machine. A V8 archive compiled for another OS, ABI
/// or architecture links successfully often enough to reach a user, and then
/// fails as a crash with no provenance attached to it.
pub fn validate_linux_package_manifest(
    manifest: &LinuxPackageManifest,
) -> Result<(), ManifestError> {
    require_equal("schema", &manifest.schema, LINUX_PACKAGE_SCHEMA_V2)?;
    validate_package_version(&manifest.version)?;
    validate_package_build_identity(
        &manifest.product_profile,
        &manifest.build_type,
        &manifest.codegen_profile,
    )?;
    validate_toolchain(&manifest.toolchain)?;
    require_equal("toolchain.sdk", &manifest.toolchain.sdk, &manifest.sysroot)?;
    validate_gles_package_graphics(&manifest.graphics)?;
    validate_migo_package_provenance(&manifest.provenance, "scripts/build-linux-sdk.sh")?;
    // x86_64 and aarch64. Baseline/features floors match
    // validate_linux_v8_target's, since a package and the V8 archive it ships
    // describe the same machine (validate_package_v8_target below cross-checks
    // the two agree, so this is the one place that also has to be right on
    // its own).
    let (expected_baseline, expected_features): (&str, &[&str]) = match manifest.arch.as_str() {
        "x86_64" => ("x86-64-v1", &["cmov", "sse2"]),
        "aarch64" => ("armv8-a", &["neon"]),
        arch => {
            return Err(ManifestError::new(format!(
                "unsupported Linux GNU package arch: {arch}"
            )));
        }
    };
    require_equal(
        "target",
        &manifest.target,
        &format!("{}-unknown-linux-gnu", manifest.arch),
    )?;
    require_equal("os", &manifest.os, "linux")?;
    // "linux" is a kernel, not an ABI. Android and OpenHarmony are Linux
    // kernels with userspaces this package cannot load against.
    require_equal("abi", &manifest.abi, "gnu")?;
    require_equal("cpu_baseline", &manifest.cpu_baseline, expected_baseline)?;
    require_sorted_unique("required_cpu_features", &manifest.required_cpu_features)?;
    if manifest.required_cpu_features != expected_features {
        return Err(ManifestError::new(format!(
            "required_cpu_features for {} must be {expected_features:?}",
            manifest.arch
        )));
    }
    require_equal("glibc_floor", &manifest.glibc_floor, LINUX_GLIBC_FLOOR)?;
    require_equal(
        "glibcxx_floor",
        &manifest.glibcxx_floor,
        LINUX_GLIBCXX_FLOOR,
    )?;
    require_non_placeholder("sysroot", &manifest.sysroot)?;

    if manifest.dynamic_dependencies.is_empty() {
        return Err(ManifestError::new(
            "dynamic_dependencies must list the shipped library's DT_NEEDED entries",
        ));
    }
    require_sorted_unique("dynamic_dependencies", &manifest.dynamic_dependencies)?;
    for dependency in &manifest.dynamic_dependencies {
        require_non_placeholder("dynamic_dependencies", dependency)?;
    }

    validate_v8_component_manifest(&manifest.v8)?;
    validate_package_v8_target(
        &manifest.v8,
        &manifest.target,
        &manifest.os,
        &manifest.abi,
        &manifest.arch,
        &manifest.cpu_baseline,
        &manifest.required_cpu_features,
    )?;
    require_equal(
        "v8.toolchain.sdk",
        &manifest.v8.toolchain.sdk,
        &manifest.sysroot,
    )?;
    require_equal(
        "v8.target.runtime_floor.glibc",
        manifest
            .v8
            .target
            .runtime_floor
            .get("glibc")
            .map(String::as_str)
            .unwrap_or(""),
        &manifest.glibc_floor,
    )?;
    require_equal(
        "v8.target.runtime_floor.glibcxx",
        manifest
            .v8
            .target
            .runtime_floor
            .get("glibcxx")
            .map(String::as_str)
            .unwrap_or(""),
        &manifest.glibcxx_floor,
    )?;

    validate_linux_snapshots(manifest)?;

    validate_package_artifacts(&manifest.artifacts)?;
    if !manifest.artifacts.contains_key("lib/libmigo.a") {
        return Err(ManifestError::new("artifacts must include lib/libmigo.a"));
    }
    let versioned_shared_object = format!("lib/libmigo.so.{}", manifest.version);
    if !manifest.artifacts.contains_key(&versioned_shared_object) {
        return Err(ManifestError::new(
            "artifacts must include lib/libmigo.so.<version> for the exact manifest version",
        ));
    }
    Ok(())
}

/// Validate a Linux package manifest and bind every declared artifact identity
/// to the regular file staged under `package_root`.
pub fn verify_linux_package(
    manifest: &LinuxPackageManifest,
    package_root: &Path,
) -> Result<(), ManifestError> {
    validate_linux_package_manifest(manifest)?;
    let manifest_path = format!("share/migo/linux-{}-manifest.json", manifest.arch);
    verify_packaged_manifest(package_root, &manifest_path, manifest)?;
    let expected_links = BTreeMap::from([
        ("lib/libmigo.so".to_string(), "libmigo.so.1".to_string()),
        (
            "lib/libmigo.so.1".to_string(),
            format!("libmigo.so.{}", manifest.version),
        ),
    ]);
    verify_package_tree(
        &manifest.artifacts,
        package_root,
        &manifest_path,
        &expected_links,
    )
}

fn validate_linux_snapshots(manifest: &LinuxPackageManifest) -> Result<(), ManifestError> {
    require_one_of(
        "snapshot_policy",
        &manifest.snapshot_policy,
        &["none", "embedded"],
    )?;
    validate_package_snapshots(
        &manifest.snapshots,
        &manifest.snapshot_policy,
        &manifest.target,
        &manifest.arch,
        &manifest.product_profile,
        &manifest.v8.hashes.archive,
    )
}

/// The snapshot rules shared by every package manifest: policy and content must
/// agree, and each snapshot must be for this package's own triple and arch,
/// because a snapshot is V8 machine code that only loads in the V8 it was made
/// for. The policy string is validated against its allowed set by the caller,
/// which differs by platform (Linux permits `none`, Android does not).
fn validate_package_snapshots(
    snapshots: &[PackageSnapshotIdentity],
    snapshot_policy: &str,
    target_triple: &str,
    arch: &str,
    product_profile: &str,
    v8_archive_hash: &str,
) -> Result<(), ManifestError> {
    // Policy and content must agree. Stating the policy is what makes "ships no
    // snapshot" a decision rather than an omission, and cross-checking it is
    // what keeps the statement true once a snapshot is added.
    match snapshot_policy {
        "none" if !snapshots.is_empty() => {
            return Err(ManifestError::new(
                "snapshot_policy is none but snapshots were listed",
            ));
        }
        "embedded" if snapshots.is_empty() => {
            return Err(ManifestError::new(
                "snapshot_policy is embedded but no snapshot was listed",
            ));
        }
        _ => {}
    }

    let mut kinds: HashSet<&str> = HashSet::new();
    for snapshot in snapshots {
        require_one_of(
            "snapshot.runtime_kind",
            &snapshot.runtime_kind,
            &["host", "worker"],
        )?;
        if !kinds.insert(snapshot.runtime_kind.as_str()) {
            return Err(ManifestError::new(format!(
                "duplicate snapshot runtime_kind: {}",
                snapshot.runtime_kind
            )));
        }
        // A snapshot is V8 machine code: one built for another triple or arch is
        // not merely suboptimal, it is not loadable.
        require_equal(
            "snapshot.target_triple",
            &snapshot.target_triple,
            target_triple,
        )?;
        require_equal("snapshot.arch", &snapshot.arch, arch)?;
        require_equal(
            "snapshot.product_profile",
            &snapshot.product_profile,
            product_profile,
        )?;
        require_equal("snapshot.schema", &snapshot.schema, "3")?;
        require_non_placeholder("snapshot.generator", &snapshot.generator)?;
        require_equal(
            "snapshot.generation_cpu_policy",
            &snapshot.generation_cpu_policy,
            "target-baseline",
        )?;
        require_sorted_unique(
            "snapshot.normalized_parameters",
            &snapshot.normalized_parameters,
        )?;
        if snapshot.normalized_parameters.is_empty() {
            return Err(ManifestError::new(
                "snapshot.normalized_parameters must record the generation parameters",
            ));
        }
        for parameter in &snapshot.normalized_parameters {
            require_non_placeholder("snapshot.normalized_parameters", parameter)?;
        }
        for required in [
            format!("--arch={arch}"),
            "--cpu-policy=target-baseline".to_string(),
            format!("--product-profile={product_profile}"),
            format!("--runtime-kind={}", snapshot.runtime_kind),
        ] {
            if snapshot
                .normalized_parameters
                .binary_search_by(|value| value.as_bytes().cmp(required.as_bytes()))
                .is_err()
            {
                return Err(ManifestError::new(format!(
                    "snapshot.normalized_parameters is missing {required}"
                )));
            }
        }
        require_sha256(
            "snapshot.external_references_hash",
            &snapshot.external_references_hash,
        )?;
        require_sha256(
            "snapshot.bootstrap_inputs_hash",
            &snapshot.bootstrap_inputs_hash,
        )?;
        if snapshot.features.is_empty() {
            return Err(ManifestError::new(
                "snapshot.features must record the generated product surface",
            ));
        }
        require_sorted_unique("snapshot.features", &snapshot.features)?;
        require_sha256("snapshot.features_hash", &snapshot.features_hash)?;
        require_sha256("snapshot.rust_sources_hash", &snapshot.rust_sources_hash)?;
        require_sha256("snapshot.v8_archive_hash", &snapshot.v8_archive_hash)?;
        require_equal(
            "snapshot.v8_archive_hash",
            &snapshot.v8_archive_hash,
            v8_archive_hash,
        )?;
        if snapshot.bytes_size == 0 {
            return Err(ManifestError::new(
                "snapshot.bytes_size must be greater than zero",
            ));
        }
        require_sha256("snapshot.bytes_hash", &snapshot.bytes_hash)?;
        require_sha256("snapshot.js_sources_hash", &snapshot.js_sources_hash)?;
        require_non_placeholder("snapshot.deno_core_version", &snapshot.deno_core_version)?;
    }
    Ok(())
}

/// Validate one Android C ABI package slice.
///
/// Same north star as the Linux validator -- the library and the things built
/// into it must describe the same machine -- with Android's differences pinned:
/// the `android_api` floor is the project minimum, the artifact is a static
/// library, and a snapshot is always embedded rather than absent.
pub fn validate_android_package_manifest(
    manifest: &AndroidPackageManifest,
) -> Result<(), ManifestError> {
    require_equal("schema", &manifest.schema, ANDROID_PACKAGE_SCHEMA_V2)?;
    validate_package_version(&manifest.version)?;
    validate_package_build_identity(
        &manifest.product_profile,
        &manifest.build_type,
        &manifest.codegen_profile,
    )?;
    validate_toolchain(&manifest.toolchain)?;
    validate_gles_package_graphics(&manifest.graphics)?;
    validate_migo_package_provenance(&manifest.provenance, "scripts/build-android-sdk.sh")?;
    require_equal("os", &manifest.os, "android")?;
    // "android" is the userspace ABI, distinct from the Linux kernel it runs on
    // and from OpenHarmony. It is what a static library built here can be linked
    // against.
    require_equal("abi", &manifest.abi, "android")?;
    require_equal(
        "min_android_api",
        &manifest.min_android_api,
        ANDROID_API_FLOOR,
    )?;

    // arch determines triple, ABI name, CPU baseline and features together;
    // splitting them lets a mismatch through.
    match manifest.arch.as_str() {
        "aarch64" => {
            require_equal(
                "target_triple",
                &manifest.target_triple,
                "aarch64-linux-android",
            )?;
            require_equal("android_abi", &manifest.android_abi, "arm64-v8a")?;
            require_equal("cpu_baseline", &manifest.cpu_baseline, "armv8-a")?;
            require_sorted_unique("required_cpu_features", &manifest.required_cpu_features)?;
            if manifest.required_cpu_features != ["neon"] {
                return Err(ManifestError::new(
                    "required_cpu_features for aarch64 must be [\"neon\"]",
                ));
            }
        }
        "x86_64" => {
            require_equal(
                "target_triple",
                &manifest.target_triple,
                "x86_64-linux-android",
            )?;
            require_equal("android_abi", &manifest.android_abi, "x86_64")?;
            require_equal("cpu_baseline", &manifest.cpu_baseline, "x86-64-v1")?;
            require_sorted_unique("required_cpu_features", &manifest.required_cpu_features)?;
            if manifest.required_cpu_features != ["cmov", "sse2"] {
                return Err(ManifestError::new(
                    "required_cpu_features for x86_64 must be [\"cmov\", \"sse2\"]",
                ));
            }
        }
        other => {
            return Err(ManifestError::new(format!(
                "unsupported Android package arch: {other}"
            )));
        }
    }

    if manifest.link_libraries.is_empty() {
        return Err(ManifestError::new(
            "link_libraries must list the system libraries the consumer links the static archive against",
        ));
    }
    require_sorted_unique("link_libraries", &manifest.link_libraries)?;
    for library in &manifest.link_libraries {
        require_non_placeholder("link_libraries", library)?;
    }

    validate_v8_component_manifest(&manifest.v8)?;
    require_equal(
        "v8.toolchain.sdk",
        &manifest.v8.toolchain.sdk,
        &manifest.toolchain.sdk,
    )?;
    validate_package_v8_target(
        &manifest.v8,
        &manifest.target_triple,
        &manifest.os,
        &manifest.abi,
        &manifest.arch,
        &manifest.cpu_baseline,
        &manifest.required_cpu_features,
    )?;
    require_equal(
        "v8.target.runtime_floor.android_api",
        manifest
            .v8
            .target
            .runtime_floor
            .get("android_api")
            .map(String::as_str)
            .unwrap_or(""),
        &manifest.min_android_api,
    )?;

    // Android always embeds a snapshot; `none` is a Linux-only policy.
    require_equal("snapshot_policy", &manifest.snapshot_policy, "embedded")?;
    validate_package_snapshots(
        &manifest.snapshots,
        &manifest.snapshot_policy,
        &manifest.target_triple,
        &manifest.arch,
        &manifest.product_profile,
        &manifest.v8.hashes.archive,
    )?;

    validate_package_artifacts(&manifest.artifacts)?;
    if !manifest.artifacts.contains_key("lib/libmigo_capi.a") {
        return Err(ManifestError::new(
            "artifacts must include lib/libmigo_capi.a",
        ));
    }
    Ok(())
}

/// Validate an Android C ABI package manifest and verify the staged static
/// library bytes it identifies.
pub fn verify_android_package(
    manifest: &AndroidPackageManifest,
    package_root: &Path,
) -> Result<(), ManifestError> {
    validate_android_package_manifest(manifest)?;
    let manifest_path = format!("share/migo/android-{}-manifest.json", manifest.android_abi);
    verify_packaged_manifest(package_root, &manifest_path, manifest)?;
    verify_package_tree(
        &manifest.artifacts,
        package_root,
        &manifest_path,
        &BTreeMap::new(),
    )
}

fn validate_package_artifacts(
    artifacts: &BTreeMap<String, PackageArtifactIdentity>,
) -> Result<(), ManifestError> {
    if artifacts.is_empty() {
        return Err(ManifestError::new("artifacts must not be empty"));
    }
    for (path, identity) in artifacts {
        validate_package_path("artifacts path", path)?;
        if identity.size_bytes == 0 {
            return Err(ManifestError::new(format!(
                "artifacts.{path}.size_bytes is 0, which cannot describe a shipped binary"
            )));
        }
        require_sha256("artifacts.sha256", &identity.sha256)?;
    }
    Ok(())
}

fn validate_package_build_identity(
    product_profile: &str,
    build_type: &str,
    codegen_profile: &str,
) -> Result<(), ManifestError> {
    require_one_of("product_profile", product_profile, &["full", "slim"])?;
    // SDK artifacts are release inputs. Accepting a debug package under this
    // schema would make the performance and hardening properties unknowable to
    // consumers even if every ABI field matched.
    require_equal("build_type", build_type, "release")?;
    require_one_of("codegen_profile", codegen_profile, &["z", "2", "3"])
}

/// Validate the package version as SemVer without accepting path, shell, or
/// generated-build-file syntax as part of a version-derived file name.
///
/// Keeping this parser local avoids adding a release-tool dependency solely
/// for three numeric components and dot-separated identifiers. It implements
/// the SemVer 2.0.0 grammar needed by Cargo package versions, including the
/// leading-zero rule for core and numeric pre-release identifiers.
fn validate_package_version(version: &str) -> Result<(), ManifestError> {
    require_non_placeholder("version", version)?;
    if version.len() > 128 || !version.is_ascii() {
        return Err(ManifestError::new(
            "version must be an ASCII SemVer no longer than 128 bytes",
        ));
    }

    let (without_build, build) = split_once_unique(version, '+', "version build metadata")?;
    if let Some(build) = build {
        validate_semver_identifiers(build, true, "version build metadata")?;
    }
    let (core, prerelease) = match without_build.split_once('-') {
        Some((core, prerelease)) if !core.is_empty() && !prerelease.is_empty() => {
            (core, Some(prerelease))
        }
        Some(_) => {
            return Err(ManifestError::new("version pre-release must not be empty"));
        }
        None => (without_build, None),
    };
    if let Some(prerelease) = prerelease {
        validate_semver_identifiers(prerelease, false, "version pre-release")?;
    }

    let mut components = core.split('.');
    for field in ["major", "minor", "patch"] {
        let component = components
            .next()
            .ok_or_else(|| ManifestError::new("version must contain major.minor.patch"))?;
        if component.is_empty()
            || !component.bytes().all(|byte| byte.is_ascii_digit())
            || (component.len() > 1 && component.starts_with('0'))
        {
            return Err(ManifestError::new(format!(
                "version {field} must be a decimal integer without leading zeroes"
            )));
        }
    }
    if components.next().is_some() {
        return Err(ManifestError::new(
            "version must contain exactly major.minor.patch",
        ));
    }
    Ok(())
}

fn split_once_unique<'a>(
    value: &'a str,
    separator: char,
    field: &str,
) -> Result<(&'a str, Option<&'a str>), ManifestError> {
    let Some((head, tail)) = value.split_once(separator) else {
        return Ok((value, None));
    };
    if head.is_empty() || tail.is_empty() || tail.contains(separator) {
        return Err(ManifestError::new(format!(
            "{field} must occur at most once and must not be empty"
        )));
    }
    Ok((head, Some(tail)))
}

fn validate_semver_identifiers(
    value: &str,
    numeric_leading_zeroes_allowed: bool,
    field: &str,
) -> Result<(), ManifestError> {
    for identifier in value.split('.') {
        if identifier.is_empty()
            || !identifier
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(ManifestError::new(format!(
                "{field} must contain non-empty ASCII alphanumeric/hyphen identifiers"
            )));
        }
        if !numeric_leading_zeroes_allowed
            && identifier.len() > 1
            && identifier.bytes().all(|byte| byte.is_ascii_digit())
            && identifier.starts_with('0')
        {
            return Err(ManifestError::new(format!(
                "numeric {field} identifiers must not contain leading zeroes"
            )));
        }
    }
    Ok(())
}

fn validate_package_v8_target(
    component: &V8ComponentManifest,
    target_triple: &str,
    os: &str,
    abi: &str,
    arch: &str,
    cpu_baseline: &str,
    required_cpu_features: &[String],
) -> Result<(), ManifestError> {
    let target = &component.target;
    for (field, actual, expected) in [
        ("v8.target.triple", target.triple.as_str(), target_triple),
        ("v8.target.os", target.os.as_str(), os),
        ("v8.target.abi", target.abi.as_str(), abi),
        ("v8.target.arch", target.arch.as_str(), arch),
        (
            "v8.target.cpu_baseline",
            target.cpu_baseline.as_str(),
            cpu_baseline,
        ),
    ] {
        if actual != expected {
            return Err(ManifestError::new(format!(
                "{field} {actual} does not match package target {expected}: a V8 built for one OS/ABI/arch/CPU baseline cannot be shipped for another"
            )));
        }
    }
    if target.required_cpu_features != required_cpu_features {
        return Err(ManifestError::new(format!(
            "v8.target.required_cpu_features {:?} do not match package features {:?}",
            target.required_cpu_features, required_cpu_features
        )));
    }
    Ok(())
}

fn verify_package_tree(
    artifacts: &BTreeMap<String, PackageArtifactIdentity>,
    package_root: &Path,
    manifest_path: &str,
    expected_symlinks: &BTreeMap<String, String>,
) -> Result<(), ManifestError> {
    let root_metadata = fs::symlink_metadata(package_root).map_err(|error| {
        ManifestError::new(format!(
            "read package root {}: {error}",
            package_root.display()
        ))
    })?;
    if !root_metadata.file_type().is_dir() {
        return Err(ManifestError::new(format!(
            "package root {} must be a directory, not a symlink or file",
            package_root.display()
        )));
    }

    for (relative_path, identity) in artifacts {
        // Validation above guarantees a relative path made exclusively of
        // normal components, so joining cannot escape package_root.
        let path = package_root.join(relative_path);
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            ManifestError::new(format!("read staged artifact {}: {error}", path.display()))
        })?;
        if !metadata.file_type().is_file() {
            return Err(ManifestError::new(format!(
                "staged artifact {} must be a regular file, not a symlink or directory",
                path.display()
            )));
        }
        if metadata.len() != identity.size_bytes {
            return Err(ManifestError::new(format!(
                "artifacts.{relative_path}.size_bytes mismatch (manifest={}, file={})",
                identity.size_bytes,
                metadata.len()
            )));
        }
        let actual_sha256 = sha256_file(&path)?;
        if actual_sha256 != identity.sha256 {
            return Err(ManifestError::new(format!(
                "artifacts.{relative_path}.sha256 mismatch (manifest={}, file={actual_sha256})",
                identity.sha256
            )));
        }
    }

    let mut directories = vec![package_root.to_path_buf()];
    let mut observed_symlinks = HashSet::new();
    while let Some(directory) = directories.pop() {
        let entries = fs::read_dir(&directory).map_err(|error| {
            ManifestError::new(format!(
                "read package directory {}: {error}",
                directory.display()
            ))
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                ManifestError::new(format!(
                    "read package directory entry under {}: {error}",
                    directory.display()
                ))
            })?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                ManifestError::new(format!("read package entry {}: {error}", path.display()))
            })?;
            let relative = package_relative_path(package_root, &path)?;
            let file_type = metadata.file_type();
            if file_type.is_dir() {
                directories.push(path);
            } else if file_type.is_file() {
                if relative != manifest_path && !artifacts.contains_key(&relative) {
                    return Err(ManifestError::new(format!(
                        "package contains undeclared regular file: {relative}"
                    )));
                }
            } else if file_type.is_symlink() {
                let Some(expected_target) = expected_symlinks.get(&relative) else {
                    return Err(ManifestError::new(format!(
                        "package contains undeclared symlink: {relative}"
                    )));
                };
                let target = fs::read_link(&path).map_err(|error| {
                    ManifestError::new(format!("read package symlink {}: {error}", path.display()))
                })?;
                if target != Path::new(expected_target) {
                    return Err(ManifestError::new(format!(
                        "package symlink target mismatch for {relative} (expected={expected_target}, actual={})",
                        target.display()
                    )));
                }
                observed_symlinks.insert(relative);
            } else {
                return Err(ManifestError::new(format!(
                    "package contains unsupported special file: {relative}"
                )));
            }
        }
    }

    for path in expected_symlinks.keys() {
        if !observed_symlinks.contains(path) {
            return Err(ManifestError::new(format!(
                "package is missing required symlink: {path}"
            )));
        }
    }
    Ok(())
}

fn verify_packaged_manifest<T>(
    package_root: &Path,
    relative_path: &str,
    expected: &T,
) -> Result<(), ManifestError>
where
    T: DeserializeOwned + PartialEq,
{
    let path = package_root.join(relative_path);
    let metadata = fs::symlink_metadata(&path).map_err(|error| {
        ManifestError::new(format!(
            "read packaged manifest {}: {error}",
            path.display()
        ))
    })?;
    if !metadata.file_type().is_file() {
        return Err(ManifestError::new(format!(
            "packaged manifest {} must be a regular file",
            path.display()
        )));
    }
    let bytes = fs::read(&path).map_err(|error| {
        ManifestError::new(format!(
            "read packaged manifest {}: {error}",
            path.display()
        ))
    })?;
    let actual: T = serde_json::from_slice(&bytes).map_err(|error| {
        ManifestError::new(format!(
            "parse packaged manifest {}: {error}",
            path.display()
        ))
    })?;
    if &actual != expected {
        return Err(ManifestError::new(format!(
            "packaged manifest {} does not match the manifest being verified",
            path.display()
        )));
    }
    Ok(())
}

fn package_relative_path(package_root: &Path, path: &Path) -> Result<String, ManifestError> {
    let relative = path.strip_prefix(package_root).map_err(|error| {
        ManifestError::new(format!(
            "package entry {} is outside root {}: {error}",
            path.display(),
            package_root.display()
        ))
    })?;
    let mut parts = Vec::new();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return Err(ManifestError::new(format!(
                "package entry has a non-normal path component: {}",
                relative.display()
            )));
        };
        let part = part.to_str().ok_or_else(|| {
            ManifestError::new(format!(
                "package entry path must be valid UTF-8: {}",
                relative.display()
            ))
        })?;
        parts.push(part);
    }
    Ok(parts.join("/"))
}

pub fn seal_v8_component_manifest(manifest: &mut V8ComponentManifest) -> Result<(), ManifestError> {
    manifest.component_id = recompute_v8_component_id(manifest)?;
    Ok(())
}

pub fn recompute_v8_component_id(manifest: &V8ComponentManifest) -> Result<String, ManifestError> {
    Ok(sha256_bytes(&canonical_json_without_field(
        manifest,
        "component_id",
        "V8 component manifest",
    )?))
}

pub fn validate_v8_component_manifest(manifest: &V8ComponentManifest) -> Result<(), ManifestError> {
    require_equal(
        "V8 component schema",
        &manifest.schema,
        V8_COMPONENT_SCHEMA_V1,
    )?;
    validate_v8_component_target(&manifest.target)?;
    validate_toolchain(&manifest.toolchain)?;
    validate_v8_runtime(&manifest.runtime)?;
    require_sha256("V8 component hashes.archive", &manifest.hashes.archive)?;
    require_sha256(
        "V8 component hashes.rust_binding",
        &manifest.hashes.rust_binding,
    )?;
    validate_provenance(&manifest.provenance)?;
    let rusty_v8_revision = require_option(
        "runtime.rusty_v8_revision",
        &manifest.runtime.rusty_v8_revision,
    )?;
    require_equal(
        "V8 component provenance.source_revision",
        &manifest.provenance.source_revision,
        rusty_v8_revision,
    )?;

    require_sha256("component_id", &manifest.component_id)?;
    let expected = recompute_v8_component_id(manifest)?;
    if manifest.component_id != expected {
        return Err(ManifestError::new(format!(
            "component_id mismatch (manifest={}, computed={expected})",
            manifest.component_id
        )));
    }
    Ok(())
}

pub fn verify_v8_component_files(
    manifest: &V8ComponentManifest,
    archive_path: &Path,
    binding_path: &Path,
) -> Result<(), ManifestError> {
    validate_v8_component_manifest(manifest)?;
    let archive_hash = sha256_file(archive_path)?;
    if archive_hash != manifest.hashes.archive {
        return Err(ManifestError::new(format!(
            "V8 component archive hash mismatch (manifest={}, computed={archive_hash})",
            manifest.hashes.archive
        )));
    }
    let binding_hash = sha256_file(binding_path)?;
    if binding_hash != manifest.hashes.rust_binding {
        return Err(ManifestError::new(format!(
            "V8 component rust_binding hash mismatch (manifest={}, computed={binding_hash})",
            manifest.hashes.rust_binding
        )));
    }
    Ok(())
}

pub fn build_package_index(
    product_profile: &str,
    sources: &[SliceManifestSource],
) -> Result<PackageIndex, ManifestError> {
    require_one_of("product_profile", product_profile, &["full", "slim"])?;
    if sources.is_empty() {
        return Err(ManifestError::new(
            "package index must reference at least one slice manifest",
        ));
    }

    let mut slices = Vec::with_capacity(sources.len());
    let mut build_type: Option<String> = None;
    let mut codegen_profile: Option<String> = None;
    for source in sources {
        validate_package_path("manifest_path", &source.package_path)?;
        let bytes = fs::read(&source.file_path).map_err(|error| {
            ManifestError::new(format!(
                "read slice manifest {}: {error}",
                source.file_path.display()
            ))
        })?;
        let manifest = parse_slice_manifest(&bytes, &source.file_path)?;
        if manifest.product_profile != product_profile {
            return Err(ManifestError::new(format!(
                "slice {} product_profile must be {product_profile:?}, got {:?}",
                source.file_path.display(),
                manifest.product_profile
            )));
        }
        if let Some(expected) = &build_type {
            require_equal("package index build_type", &manifest.build_type, expected)?;
        } else {
            build_type = Some(manifest.build_type.clone());
        }
        if let Some(expected) = &codegen_profile {
            require_equal(
                "package index codegen_profile",
                &manifest.codegen_profile,
                expected,
            )?;
        } else {
            codegen_profile = Some(manifest.codegen_profile.clone());
        }
        slices.push(SliceIndexEntry {
            target_triple: manifest.target.triple,
            arch: manifest.target.arch,
            manifest_path: source.package_path.clone(),
            manifest_sha256: sha256_bytes(&bytes),
            artifact_id: manifest.artifact_id,
        });
    }
    slices.sort_unstable_by(|left, right| slice_entry_key(left).cmp(&slice_entry_key(right)));

    let index = PackageIndex {
        schema: PACKAGE_INDEX_SCHEMA_V1.to_string(),
        product_profile: product_profile.to_string(),
        build_type: build_type.expect("non-empty sources establish build_type"),
        codegen_profile: codegen_profile.expect("non-empty sources establish codegen_profile"),
        slices,
    };
    validate_package_index_shape(&index)?;
    Ok(index)
}

pub fn verify_package_index(index: &PackageIndex, root: &Path) -> Result<(), ManifestError> {
    validate_package_index_shape(index)?;
    let canonical_root = root.canonicalize().map_err(|error| {
        ManifestError::new(format!(
            "canonicalize package root {}: {error}",
            root.display()
        ))
    })?;

    for entry in &index.slices {
        let manifest_path = root.join(&entry.manifest_path);
        let canonical_manifest = manifest_path.canonicalize().map_err(|error| {
            ManifestError::new(format!(
                "canonicalize slice manifest {}: {error}",
                manifest_path.display()
            ))
        })?;
        if !canonical_manifest.starts_with(&canonical_root) {
            return Err(ManifestError::new(format!(
                "manifest_path escapes package root: {}",
                entry.manifest_path
            )));
        }
        let bytes = fs::read(&canonical_manifest).map_err(|error| {
            ManifestError::new(format!(
                "read slice manifest {}: {error}",
                canonical_manifest.display()
            ))
        })?;
        let actual_hash = sha256_bytes(&bytes);
        if actual_hash != entry.manifest_sha256 {
            return Err(ManifestError::new(format!(
                "manifest_sha256 mismatch for {} (index={}, computed={actual_hash})",
                entry.manifest_path, entry.manifest_sha256
            )));
        }
        let manifest = parse_slice_manifest(&bytes, &canonical_manifest)?;
        require_equal(
            "package index product_profile",
            &manifest.product_profile,
            &index.product_profile,
        )?;
        require_equal(
            "package index build_type",
            &manifest.build_type,
            &index.build_type,
        )?;
        require_equal(
            "package index codegen_profile",
            &manifest.codegen_profile,
            &index.codegen_profile,
        )?;
        require_equal(
            "package index target_triple",
            &manifest.target.triple,
            &entry.target_triple,
        )?;
        require_equal("package index arch", &manifest.target.arch, &entry.arch)?;
        require_equal(
            "package index artifact_id",
            &manifest.artifact_id,
            &entry.artifact_id,
        )?;
    }
    Ok(())
}

pub fn build_release_attestation(
    package_path: &Path,
    package_index_path: &Path,
) -> Result<ReleaseAttestation, ManifestError> {
    let package_file = file_name("package_file", package_path)?;
    let package_index_file = file_name("package_index_file", package_index_path)?;
    let metadata = package_path.metadata().map_err(|error| {
        ManifestError::new(format!(
            "read final package metadata {}: {error}",
            package_path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(ManifestError::new(format!(
            "final package is not a regular file: {}",
            package_path.display()
        )));
    }

    let attestation = ReleaseAttestation {
        schema: RELEASE_ATTESTATION_SCHEMA_V1.to_string(),
        package_file,
        package_size_bytes: metadata.len(),
        package_sha256: sha256_file(package_path)?,
        package_index_file,
        package_index_sha256: sha256_file(package_index_path)?,
    };
    validate_release_attestation_shape(&attestation)?;
    Ok(attestation)
}

pub fn verify_release_attestation(
    attestation: &ReleaseAttestation,
    package_path: &Path,
    package_index_path: &Path,
) -> Result<(), ManifestError> {
    validate_release_attestation_shape(attestation)?;
    require_equal(
        "package_file",
        &file_name("package_file", package_path)?,
        &attestation.package_file,
    )?;
    require_equal(
        "package_index_file",
        &file_name("package_index_file", package_index_path)?,
        &attestation.package_index_file,
    )?;

    let size = package_path
        .metadata()
        .map_err(|error| {
            ManifestError::new(format!(
                "read final package metadata {}: {error}",
                package_path.display()
            ))
        })?
        .len();
    if size != attestation.package_size_bytes {
        return Err(ManifestError::new(format!(
            "package_size_bytes mismatch (attestation={}, computed={size})",
            attestation.package_size_bytes
        )));
    }
    let package_hash = sha256_file(package_path)?;
    if package_hash != attestation.package_sha256 {
        return Err(ManifestError::new(format!(
            "package_sha256 mismatch (attestation={}, computed={package_hash})",
            attestation.package_sha256
        )));
    }
    let index_hash = sha256_file(package_index_path)?;
    if index_hash != attestation.package_index_sha256 {
        return Err(ManifestError::new(format!(
            "package_index_sha256 mismatch (attestation={}, computed={index_hash})",
            attestation.package_index_sha256
        )));
    }
    Ok(())
}

pub fn sha256_file(path: &Path) -> Result<String, ManifestError> {
    let mut file = File::open(path).map_err(|error| {
        ManifestError::new(format!("open file for hashing {}: {error}", path.display()))
    })?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|error| {
            ManifestError::new(format!("read file for hashing {}: {error}", path.display()))
        })?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format_digest(digest.finalize()))
}

fn parse_slice_manifest(bytes: &[u8], path: &Path) -> Result<SliceManifest, ManifestError> {
    let manifest: SliceManifest = serde_json::from_slice(bytes).map_err(|error| {
        ManifestError::new(format!("parse slice manifest {}: {error}", path.display()))
    })?;
    validate_slice_manifest(&manifest)?;
    Ok(manifest)
}

fn validate_package_index_shape(index: &PackageIndex) -> Result<(), ManifestError> {
    require_equal(
        "package index schema",
        &index.schema,
        PACKAGE_INDEX_SCHEMA_V1,
    )?;
    require_one_of(
        "package index product_profile",
        &index.product_profile,
        &["full", "slim"],
    )?;
    require_one_of(
        "package index build_type",
        &index.build_type,
        &["debug", "release"],
    )?;
    require_one_of(
        "package index codegen_profile",
        &index.codegen_profile,
        &["z", "2", "3"],
    )?;
    if index.build_type == "debug" && index.codegen_profile != "z" {
        return Err(ManifestError::new(
            "package index codegen_profile 2/3 requires a release build",
        ));
    }
    if index.slices.is_empty() {
        return Err(ManifestError::new(
            "package index must reference at least one slice manifest",
        ));
    }

    let mut sorted = index.slices.clone();
    sorted.sort_unstable_by(|left, right| slice_entry_key(left).cmp(&slice_entry_key(right)));
    if sorted != index.slices {
        return Err(ManifestError::new(
            "package index slices must be sorted by target, architecture, and path",
        ));
    }

    let mut paths = HashSet::new();
    let mut targets = HashSet::new();
    for entry in &index.slices {
        validate_package_path("manifest_path", &entry.manifest_path)?;
        require_sha256("manifest_sha256", &entry.manifest_sha256)?;
        require_sha256("artifact_id", &entry.artifact_id)?;
        match entry.arch.as_str() {
            "aarch64" => require_equal(
                "package index target_triple",
                &entry.target_triple,
                "aarch64-linux-android",
            )?,
            "x86_64" => require_equal(
                "package index target_triple",
                &entry.target_triple,
                "x86_64-linux-android",
            )?,
            other => {
                return Err(ManifestError::new(format!(
                    "unsupported package index architecture: {other}"
                )));
            }
        }
        if !paths.insert(entry.manifest_path.as_str()) {
            return Err(ManifestError::new(format!(
                "duplicate package manifest_path: {}",
                entry.manifest_path
            )));
        }
        if !targets.insert((entry.target_triple.as_str(), entry.arch.as_str())) {
            return Err(ManifestError::new(format!(
                "duplicate package target slice: {}/{}",
                entry.target_triple, entry.arch
            )));
        }
    }
    Ok(())
}

fn validate_release_attestation_shape(
    attestation: &ReleaseAttestation,
) -> Result<(), ManifestError> {
    require_equal(
        "release attestation schema",
        &attestation.schema,
        RELEASE_ATTESTATION_SCHEMA_V1,
    )?;
    validate_file_name("package_file", &attestation.package_file)?;
    validate_file_name("package_index_file", &attestation.package_index_file)?;
    require_sha256("package_sha256", &attestation.package_sha256)?;
    require_sha256("package_index_sha256", &attestation.package_index_sha256)
}

fn validate_package_path(field: &str, path: &str) -> Result<(), ManifestError> {
    if path.is_empty() || path.contains('\\') {
        return Err(ManifestError::new(format!(
            "{field} must be a non-empty relative package path using '/'"
        )));
    }
    let parsed = Path::new(path);
    if parsed.is_absolute()
        || parsed
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ManifestError::new(format!(
            "{field} must be a safe relative package path: {path}"
        )));
    }
    Ok(())
}

fn validate_file_name(field: &str, value: &str) -> Result<(), ManifestError> {
    require_non_placeholder(field, value)?;
    if Path::new(value).file_name().and_then(|name| name.to_str()) != Some(value) {
        return Err(ManifestError::new(format!(
            "{field} must contain only a file name"
        )));
    }
    Ok(())
}

fn file_name(field: &str, path: &Path) -> Result<String, ManifestError> {
    let value = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| ManifestError::new(format!("{field} must be valid UTF-8")))?
        .to_string();
    validate_file_name(field, &value)?;
    Ok(value)
}

fn slice_entry_key(entry: &SliceIndexEntry) -> (&[u8], &[u8], &[u8]) {
    (
        entry.target_triple.as_bytes(),
        entry.arch.as_bytes(),
        entry.manifest_path.as_bytes(),
    )
}

fn validate_android_target(target: &TargetIdentity) -> Result<(), ManifestError> {
    require_equal("target.os", &target.os, "android")?;
    require_equal("target.abi", &target.abi, "android")?;
    if target.runtime_floor.len() != 1
        || target.runtime_floor.get("android_api").map(String::as_str) != Some("26")
    {
        return Err(ManifestError::new(
            "target.runtime_floor.android_api must be exactly 26",
        ));
    }

    match target.arch.as_str() {
        "aarch64" => {
            require_equal("target.triple", &target.triple, "aarch64-linux-android")?;
            require_equal("target.cpu_baseline", &target.cpu_baseline, "armv8-a")?;
            require_sorted_unique(
                "target.required_cpu_features",
                &target.required_cpu_features,
            )?;
            if target.required_cpu_features != ["neon"] {
                return Err(ManifestError::new(
                    "target.required_cpu_features for aarch64 must be [\"neon\"]",
                ));
            }
        }
        "x86_64" => {
            require_equal("target.triple", &target.triple, "x86_64-linux-android")?;
            require_equal("target.cpu_baseline", &target.cpu_baseline, "x86-64-v1")?;
            require_sorted_unique(
                "target.required_cpu_features",
                &target.required_cpu_features,
            )?;
            if target.required_cpu_features != ["cmov", "sse2"] {
                return Err(ManifestError::new(
                    "target.required_cpu_features for x86_64 must be [\"cmov\", \"sse2\"]",
                ));
            }
        }
        other => {
            return Err(ManifestError::new(format!(
                "unsupported target.arch for Android v1: {other}"
            )));
        }
    }
    Ok(())
}

fn validate_v8_component_target(target: &TargetIdentity) -> Result<(), ManifestError> {
    match (target.os.as_str(), target.abi.as_str()) {
        ("android", "android") => validate_android_target(target),
        ("linux", "gnu") => validate_linux_v8_target(target),
        ("linux", "ohos") => validate_ohos_v8_target(target),
        ("macos", "darwin") => validate_apple_v8_target(target),
        ("windows", "msvc") => validate_windows_v8_target(target),
        (os, abi) => Err(ManifestError::new(format!(
            "unsupported V8 component target OS/ABI: {os}/{abi}"
        ))),
    }
}

/// macOS, x86_64 and aarch64. There is no iOS arm and there is not meant to be:
/// on iOS the content JavaScript runs in WKWebView's WebContent process to get
/// the system JIT, so an embedded V8 there would be interpreted -- the
/// arrangement this project already rejected for in-process JavaScriptCore.
/// macOS is the one Apple platform whose public hardened-runtime entitlement
/// allows JIT, so it is the only one that ships a V8 component.
///
/// `os = "macos"` with `abi = "darwin"` because that is what the toolchain says:
/// rustc reports `target_os = "macos"` and the triple's last component is
/// `darwin`. `target_vendor = "apple"` is the wrong spelling here even though the
/// engine's conditional code selects on it -- that predicate covers iOS too, and
/// this component exists only for macOS.
///
/// The floor is an OS version rather than a library version, unlike Linux's
/// glibc/glibcxx pair: what a consumer's Mac must be new enough for is the
/// deployment target the archive was compiled against. Its single source is
/// contracts/apple/deployment-floor.json, and
/// scripts/test-apple-v8-pin-contract.sh is what holds the lock to it.
fn validate_apple_v8_target(target: &TargetIdentity) -> Result<(), ManifestError> {
    let (expected_baseline, expected_features): (&str, &[&str]) = match target.arch.as_str() {
        "x86_64" => ("x86-64-v1", &["cmov", "sse2"]),
        "aarch64" => ("armv8-a", &["neon"]),
        arch => {
            return Err(ManifestError::new(format!(
                "unsupported Apple V8 target arch: {arch}"
            )));
        }
    };
    require_equal(
        "target.triple",
        &target.triple,
        &format!("{}-apple-darwin", target.arch),
    )?;
    require_equal("target.os", &target.os, "macos")?;
    require_equal("target.abi", &target.abi, "darwin")?;
    require_equal(
        "target.cpu_baseline",
        &target.cpu_baseline,
        expected_baseline,
    )?;
    require_sorted_unique(
        "target.required_cpu_features",
        &target.required_cpu_features,
    )?;
    if target.required_cpu_features != expected_features {
        return Err(ManifestError::new(format!(
            "target.required_cpu_features for Apple {} must be {expected_features:?}",
            target.arch
        )));
    }
    if target.runtime_floor.len() != 1 || !target.runtime_floor.contains_key("macos") {
        return Err(ManifestError::new(
            "Apple V8 target.runtime_floor must be exactly one macos entry",
        ));
    }
    // `major.minor`, both decimal. Rejecting a bare major is the point rather
    // than pedantry: Apple spells its deployment targets with both components, so
    // a manifest recording only a major would compare unequal to the contract
    // while describing the same floor -- a disagreement about text presented as a
    // disagreement about the platform. The number itself is deliberately not
    // written here; contracts/apple/deployment-floor.json is its single source and
    // scripts/test-apple-deployment-floor-contract.sh fails the build when a file
    // outside the derived set carries its own copy.
    let floor = &target.runtime_floor["macos"];
    let mut parts = floor.split('.');
    let well_formed = match (parts.next(), parts.next(), parts.next()) {
        (Some(major), Some(minor), None) => {
            !major.is_empty()
                && !minor.is_empty()
                && major.bytes().all(|byte| byte.is_ascii_digit())
                && minor.bytes().all(|byte| byte.is_ascii_digit())
        }
        _ => false,
    };
    if !well_formed {
        return Err(ManifestError::new(format!(
            "Apple V8 target.runtime_floor.macos must be a major.minor version, got {floor:?}"
        )));
    }
    Ok(())
}

/// OpenHarmony, whose `os`/`abi` pair is `linux`/`ohos` because that is what the
/// compiler says: Rust reports `target_os = "linux"` with `target_env = "ohos"`, and the
/// engine's own conditional code selects on exactly that pair. Recording `os = "ohos"`
/// would be a third spelling of the platform that nothing else in the tree uses.
///
/// The floor is an API level, as on Android, and it is what V8 was *compiled against* --
/// the SDK's own sysroot -- not the higher product floor a package declares. musl rather
/// than glibc is the reason an OpenHarmony archive is not interchangeable with a Linux
/// GNU one even at the same triple prefix, so the two validators stay separate.
fn validate_ohos_v8_target(target: &TargetIdentity) -> Result<(), ManifestError> {
    let (expected_baseline, expected_features): (&str, &[&str]) = match target.arch.as_str() {
        "x86_64" => ("x86-64-v1", &["cmov", "sse2"]),
        "aarch64" => ("armv8-a", &["neon"]),
        arch => {
            return Err(ManifestError::new(format!(
                "unsupported OpenHarmony V8 target arch: {arch}"
            )));
        }
    };
    require_equal(
        "target.triple",
        &target.triple,
        &format!("{}-unknown-linux-ohos", target.arch),
    )?;
    require_equal("target.os", &target.os, "linux")?;
    require_equal("target.abi", &target.abi, "ohos")?;
    require_equal(
        "target.cpu_baseline",
        &target.cpu_baseline,
        expected_baseline,
    )?;
    require_sorted_unique(
        "target.required_cpu_features",
        &target.required_cpu_features,
    )?;
    if target.required_cpu_features != expected_features {
        return Err(ManifestError::new(format!(
            "target.required_cpu_features for OpenHarmony {} must be {expected_features:?}",
            target.arch
        )));
    }
    if target.runtime_floor.len() != 1 || !target.runtime_floor.contains_key("ohos_api") {
        return Err(ManifestError::new(
            "OpenHarmony V8 target.runtime_floor must be exactly one ohos_api entry",
        ));
    }
    let api = &target.runtime_floor["ohos_api"];
    if api.is_empty() || !api.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ManifestError::new(format!(
            "OpenHarmony V8 target.runtime_floor.ohos_api must be a decimal API level, got {api:?}"
        )));
    }
    Ok(())
}

/// Linux GNU, x86_64 and aarch64. glibc/glibcxx floors are not per-arch:
/// Debian ships one glibc release across every architecture a given suite
/// supports, so both archs are built against the same bullseye-era floor
/// (see scripts/write-linux-v8-component-manifest.py's TARGETS table for the
/// same reasoning on the Python side that produces this).
fn validate_linux_v8_target(target: &TargetIdentity) -> Result<(), ManifestError> {
    let (expected_baseline, expected_features): (&str, &[&str]) = match target.arch.as_str() {
        "x86_64" => ("x86-64-v1", &["cmov", "sse2"]),
        "aarch64" => ("armv8-a", &["neon"]),
        arch => {
            return Err(ManifestError::new(format!(
                "unsupported Linux GNU V8 target arch: {arch}"
            )));
        }
    };
    require_equal(
        "target.triple",
        &target.triple,
        &format!("{}-unknown-linux-gnu", target.arch),
    )?;
    require_equal("target.os", &target.os, "linux")?;
    require_equal("target.abi", &target.abi, "gnu")?;
    require_equal(
        "target.cpu_baseline",
        &target.cpu_baseline,
        expected_baseline,
    )?;
    require_sorted_unique(
        "target.required_cpu_features",
        &target.required_cpu_features,
    )?;
    if target.required_cpu_features != expected_features {
        return Err(ManifestError::new(format!(
            "target.required_cpu_features for Linux {} must be {expected_features:?}",
            target.arch
        )));
    }
    if target.runtime_floor.len() != 2
        || target.runtime_floor.get("glibc").map(String::as_str) != Some(LINUX_GLIBC_FLOOR)
        || target.runtime_floor.get("glibcxx").map(String::as_str) != Some(LINUX_GLIBCXX_FLOOR)
    {
        return Err(ManifestError::new(format!(
            "Linux V8 target.runtime_floor must be exactly glibc={LINUX_GLIBC_FLOOR}, glibcxx={LINUX_GLIBCXX_FLOOR}"
        )));
    }
    Ok(())
}

fn validate_windows_v8_target(target: &TargetIdentity) -> Result<(), ManifestError> {
    let (expected_baseline, expected_features): (&str, &[&str]) = match target.arch.as_str() {
        "x86_64" => ("x86-64-v1", &["cmov", "sse2"]),
        "aarch64" => ("armv8-a", &["neon"]),
        arch => {
            return Err(ManifestError::new(format!(
                "unsupported Windows V8 target arch: {arch}"
            )));
        }
    };
    require_equal(
        "target.triple",
        &target.triple,
        &format!("{}-pc-windows-msvc", target.arch),
    )?;
    require_equal("target.os", &target.os, "windows")?;
    require_equal("target.abi", &target.abi, "msvc")?;
    require_equal(
        "target.cpu_baseline",
        &target.cpu_baseline,
        expected_baseline,
    )?;
    require_sorted_unique(
        "target.required_cpu_features",
        &target.required_cpu_features,
    )?;
    if target.required_cpu_features != expected_features {
        return Err(ManifestError::new(format!(
            "target.required_cpu_features for Windows {} must be {expected_features:?}",
            target.arch
        )));
    }
    if target.runtime_floor.len() != 1
        || target.runtime_floor.get("msvc_runtime").map(String::as_str)
            != Some(WINDOWS_MSVC_RUNTIME)
    {
        return Err(ManifestError::new(format!(
            "Windows V8 target.runtime_floor must be exactly msvc_runtime={WINDOWS_MSVC_RUNTIME}"
        )));
    }
    Ok(())
}

fn validate_toolchain(toolchain: &ToolchainIdentity) -> Result<(), ManifestError> {
    require_non_placeholder("toolchain.rustc", &toolchain.rustc)?;
    require_non_placeholder("toolchain.compiler", &toolchain.compiler)?;
    require_non_placeholder("toolchain.sdk", &toolchain.sdk)?;
    require_non_placeholder("toolchain.linker", &toolchain.linker)
}

fn validate_gles_package_graphics(graphics: &GraphicsIdentity) -> Result<(), ManifestError> {
    require_equal(
        "graphics.backend_family",
        &graphics.backend_family,
        "gles-native",
    )?;
    require_equal(
        "graphics.required_api",
        &graphics.required_api,
        "OpenGL ES 3.0",
    )
}

fn validate_v8_runtime(runtime: &RuntimeIdentity) -> Result<(), ManifestError> {
    require_equal("runtime.backend", &runtime.backend, "v8")?;
    require_non_placeholder(
        "runtime.rusty_v8_version",
        require_option("runtime.rusty_v8_version", &runtime.rusty_v8_version)?,
    )?;
    require_revision(
        "runtime.rusty_v8_revision",
        require_option("runtime.rusty_v8_revision", &runtime.rusty_v8_revision)?,
    )?;
    require_revision(
        "runtime.v8_revision",
        require_option("runtime.v8_revision", &runtime.v8_revision)?,
    )?;
    if runtime.normalized_gn_args.is_empty() {
        return Err(ManifestError::new(
            "runtime.normalized_gn_args must not be empty",
        ));
    }
    require_sorted_unique("runtime.normalized_gn_args", &runtime.normalized_gn_args)?;
    let mut gn_keys = HashSet::new();
    for argument in &runtime.normalized_gn_args {
        require_non_placeholder("runtime.normalized_gn_args", argument)?;
        let (key, _) = argument.split_once('=').ok_or_else(|| {
            ManifestError::new(format!(
                "runtime.normalized_gn_args must use key=value syntax: {argument}"
            ))
        })?;
        require_non_placeholder("runtime.normalized_gn_args key", key)?;
        if !gn_keys.insert(key) {
            return Err(ManifestError::new(format!(
                "duplicate GN argument key: {key}"
            )));
        }
    }
    let mut patch_ids = HashSet::new();
    for patch in &runtime.patches {
        require_non_placeholder("runtime.patches.id", &patch.id)?;
        require_sha256("runtime.patches.sha256", &patch.sha256)?;
        if !patch_ids.insert(&patch.id) {
            return Err(ManifestError::new(format!(
                "duplicate runtime patch id: {}",
                patch.id
            )));
        }
    }
    Ok(())
}

fn validate_snapshots(
    snapshots: &[SnapshotIdentity],
    product_profile: &str,
    target: &TargetIdentity,
) -> Result<(), ManifestError> {
    if snapshots.is_empty() {
        return Err(ManifestError::new(
            "native V8 artifact must contain at least one snapshot identity",
        ));
    }
    let mut kinds: HashSet<&str> = HashSet::new();
    for snapshot in snapshots {
        require_one_of(
            "snapshot.runtime_kind",
            &snapshot.runtime_kind,
            &["host", "worker"],
        )?;
        if !kinds.insert(snapshot.runtime_kind.as_str()) {
            return Err(ManifestError::new(format!(
                "duplicate snapshot runtime_kind: {}",
                snapshot.runtime_kind
            )));
        }
        require_equal(
            "snapshot.product_profile",
            &snapshot.product_profile,
            product_profile,
        )?;
        require_equal(
            "snapshot.target_triple",
            &snapshot.target_triple,
            &target.triple,
        )?;
        require_equal("snapshot.arch", &snapshot.arch, &target.arch)?;
        if snapshot.runtime_kind == "worker" && product_profile != "full" {
            return Err(ManifestError::new(
                "worker snapshot is only valid for product_profile full",
            ));
        }
        require_non_placeholder("snapshot.schema", &snapshot.schema)?;
        require_non_placeholder("snapshot.generator", &snapshot.generator)?;
        require_equal(
            "snapshot.generation_cpu_policy",
            &snapshot.generation_cpu_policy,
            "target-baseline",
        )?;
        require_sorted_unique(
            "snapshot.normalized_parameters",
            &snapshot.normalized_parameters,
        )?;
        for parameter in &snapshot.normalized_parameters {
            require_non_placeholder("snapshot.normalized_parameters", parameter)?;
        }
        let mut expected_parameters = vec![
            format!("--arch={}", target.arch),
            "--cpu-policy=target-baseline".to_string(),
            format!("--product-profile={product_profile}"),
            format!("--runtime-kind={}", snapshot.runtime_kind),
            "--warmup=none".to_string(),
        ];
        expected_parameters.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
        if snapshot.normalized_parameters != expected_parameters {
            return Err(ManifestError::new(format!(
                "snapshot.normalized_parameters do not match target arch {} and runtime kind {}",
                target.arch, snapshot.runtime_kind
            )));
        }
        require_sha256(
            "snapshot.external_references_hash",
            &snapshot.external_references_hash,
        )?;
        require_sha256(
            "snapshot.bootstrap_inputs_hash",
            &snapshot.bootstrap_inputs_hash,
        )?;
        require_sha256("snapshot.bytes_hash", &snapshot.bytes_hash)?;
    }
    if !kinds.contains("host") {
        return Err(ManifestError::new(
            "native V8 artifact must contain a host snapshot identity",
        ));
    }
    Ok(())
}

fn validate_hashes(hashes: &ArtifactHashes) -> Result<(), ManifestError> {
    require_sha256("hashes.runtime_binary", &hashes.runtime_binary)?;
    require_sha256(
        "hashes.v8_archive",
        require_option("hashes.v8_archive", &hashes.v8_archive)?,
    )?;
    require_sha256(
        "hashes.rust_binding",
        require_option("hashes.rust_binding", &hashes.rust_binding)?,
    )?;
    // Optional, and optional in the direction that matters: the field describes
    // a `libc++_shared.so` in the payload, and there is not always one. The
    // Android packaging step ships the shared STL if and only if `libmigo.so`
    // names it in DT_NEEDED, which today none of the four Android binaries
    // does -- V8's archive carries Chromium's libc++ statically, so nothing
    // ever loaded the ~1 MB per ABI that used to be shipped beside it.
    //
    // Still validated when present: an absent field means "no C++ runtime in
    // this payload", while a malformed one means the producer is wrong about
    // what it built. `verify-android-aar-manifests.py` is what refuses the
    // remaining two mismatches -- a hash with no file, and a file with no hash.
    match &hashes.cxx_runtime {
        Some(value) => require_sha256("hashes.cxx_runtime", value),
        None => Ok(()),
    }
}

fn validate_provenance(provenance: &ProvenanceIdentity) -> Result<(), ManifestError> {
    require_revision("provenance.source_revision", &provenance.source_revision)?;
    require_non_placeholder("provenance.build_recipe", &provenance.build_recipe)?;
    require_sha256(
        "provenance.build_recipe_sha256",
        &provenance.build_recipe_sha256,
    )?;
    if provenance.licenses.is_empty() {
        return Err(ManifestError::new("provenance.licenses must not be empty"));
    }
    require_sorted_unique("provenance.licenses", &provenance.licenses)?;
    for license in &provenance.licenses {
        require_non_placeholder("provenance.licenses", license)?;
    }
    Ok(())
}

fn validate_migo_provenance(provenance: &ProvenanceIdentity) -> Result<(), ManifestError> {
    validate_provenance(provenance)?;
    if !provenance
        .licenses
        .iter()
        .any(|license| license == "BSL-1.1")
    {
        return Err(ManifestError::new(
            "Migo artifact provenance must record the repository's current BSL-1.1 license",
        ));
    }
    Ok(())
}

fn validate_migo_package_provenance(
    provenance: &ProvenanceIdentity,
    expected_recipe: &str,
) -> Result<(), ManifestError> {
    validate_migo_provenance(provenance)?;
    require_equal(
        "provenance.build_recipe",
        &provenance.build_recipe,
        expected_recipe,
    )
}

fn require_option<'a>(field: &str, value: &'a Option<String>) -> Result<&'a str, ManifestError> {
    value
        .as_deref()
        .ok_or_else(|| ManifestError::new(format!("{field} is required")))
}

fn require_equal(field: &str, actual: &str, expected: &str) -> Result<(), ManifestError> {
    if actual == expected {
        Ok(())
    } else {
        Err(ManifestError::new(format!(
            "{field} must be {expected:?}, got {actual:?}"
        )))
    }
}

fn require_one_of(field: &str, actual: &str, expected: &[&str]) -> Result<(), ManifestError> {
    if expected.contains(&actual) {
        Ok(())
    } else {
        Err(ManifestError::new(format!(
            "{field} must be one of {expected:?}, got {actual:?}"
        )))
    }
}

fn require_non_placeholder(field: &str, value: &str) -> Result<(), ManifestError> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("unknown")
        || trimmed.eq_ignore_ascii_case("unset")
        || trimmed.contains('<')
        || trimmed.contains('>')
    {
        return Err(ManifestError::new(format!(
            "{field} contains an empty, unknown, or placeholder value"
        )));
    }
    Ok(())
}

fn require_revision(field: &str, value: &str) -> Result<(), ManifestError> {
    require_non_placeholder(field, value)?;
    if value.len() != 40 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ManifestError::new(format!(
            "{field} must be a full 40-character hexadecimal revision"
        )));
    }
    Ok(())
}

fn require_sha256(field: &str, value: &str) -> Result<(), ManifestError> {
    require_non_placeholder(field, value)?;
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ManifestError::new(format!(
            "{field} must be a lowercase 64-character SHA-256"
        )));
    }
    Ok(())
}

fn require_sorted_unique(field: &str, values: &[String]) -> Result<(), ManifestError> {
    if values.is_empty() {
        return Err(ManifestError::new(format!("{field} must not be empty")));
    }
    let mut normalized = values.to_vec();
    normalized.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    normalized.dedup();
    if normalized != values {
        return Err(ManifestError::new(format!(
            "{field} must be sorted by bytes and contain no duplicates"
        )));
    }
    Ok(())
}

fn write_canonical_json(value: &Value, output: &mut String) -> Result<(), ManifestError> {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Value::Number(number) => {
            if number.is_f64() {
                return Err(ManifestError::new(
                    "floating-point values are forbidden in manifest identity",
                ));
            }
            output.push_str(&number.to_string());
        }
        Value::String(string) => output.push_str(
            &serde_json::to_string(string)
                .map_err(|error| ManifestError::new(format!("encode JSON string: {error}")))?,
        ),
        Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                write_canonical_json(value, output)?;
            }
            output.push(']');
        }
        Value::Object(object) => {
            output.push('{');
            let mut keys: Vec<_> = object.keys().collect();
            keys.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
            for (index, key) in keys.into_iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                output.push_str(&serde_json::to_string(key).map_err(|error| {
                    ManifestError::new(format!("encode JSON object key: {error}"))
                })?);
                output.push(':');
                write_canonical_json(&object[key], output)?;
            }
            output.push('}');
        }
    }
    Ok(())
}

fn canonical_json_without_field(
    value: &impl Serialize,
    omitted_field: &str,
    label: &str,
) -> Result<Vec<u8>, ManifestError> {
    let mut value = serde_json::to_value(value)
        .map_err(|error| ManifestError::new(format!("serialize {label} identity: {error}")))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| ManifestError::new(format!("{label} must serialize as an object")))?;
    object.remove(omitted_field);

    let mut output = String::new();
    write_canonical_json(&value, &mut output)?;
    Ok(output.into_bytes())
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format_digest(Sha256::digest(bytes))
}

fn format_digest(digest: impl AsRef<[u8]>) -> String {
    let digest = digest.as_ref();
    let mut output = String::with_capacity(64);
    for byte in digest {
        use fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}
