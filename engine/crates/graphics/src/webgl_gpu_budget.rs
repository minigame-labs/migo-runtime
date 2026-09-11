//! Authoritative resident-byte accounting for WebGL texture/renderbuffer storage.
//!
//! This state is owned by the ordered render thread. Binding commands and
//! allocation commands therefore cross this ledger in the same FIFO order as
//! the driver calls, without a lock on the render hot path. Only the aggregate
//! process counter is atomic because independent render owners share it.
//!
//! Follow-up owners still need to publish separate domains through the same
//! diagnostics surface: `canvas/manager/drawing_buffer.rs` for presentation
//! attachments, `canvas/manager/pbo_upload.rs` and `upload_thread.rs` for PBO
//! and staging residency, and Skia's resource cache for engine-owned backing.
//! Those files are intentionally not changed here because their ownership and
//! device-residency semantics require their owning threads (and device data).
//! Handler paths that still require conversion to this authority are
//! `TexImage2DFromShared`, `TexImage2DFromSnapshot`,
//! `TexImage2DFromTextCache`, and `TexImage2DFromCanvas2D` in
//! `renderergl/handler.rs`, plus `BufferData` in that handler. Their manager
//! implementations currently allocate/copy through engine-owned GL paths.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use shared::protocol::render_cmd::{CanvasId, RenderbufferId, TextureId};

const GL_TEXTURE0: u32 = 0x84C0;
const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_TEXTURE_3D: u32 = 0x806F;
const GL_TEXTURE_CUBE_MAP: u32 = 0x8513;
const GL_TEXTURE_CUBE_MAP_POSITIVE_X: u32 = 0x8515;
const GL_TEXTURE_CUBE_MAP_NEGATIVE_Z: u32 = 0x851A;
const GL_TEXTURE_2D_ARRAY: u32 = 0x8C1A;
const GL_RENDERBUFFER: u32 = 0x8D41;
const WEBGL_TEXTURE_UNIT_COUNT: u32 = 32;

const MAX_CONTEXT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_PROCESS_BYTES: u64 = 512 * 1024 * 1024;
const MAX_2D_DIMENSION: u32 = 16_384;
const MAX_3D_DIMENSION: u32 = 2_048;
const MAX_ARRAY_LAYERS: u32 = 2_048;
const MAX_SAMPLES: u32 = 16;

#[derive(Clone, Copy, Debug)]
pub(crate) struct GpuBudgetLimits {
    pub(crate) max_context_bytes: u64,
    pub(crate) max_process_bytes: u64,
    pub(crate) max_2d_dimension: u32,
    pub(crate) max_3d_dimension: u32,
    pub(crate) max_array_layers: u32,
    pub(crate) max_samples: u32,
}

impl GpuBudgetLimits {
    const PRODUCTION: Self = Self {
        max_context_bytes: MAX_CONTEXT_BYTES,
        max_process_bytes: MAX_PROCESS_BYTES,
        max_2d_dimension: MAX_2D_DIMENSION,
        max_3d_dimension: MAX_3D_DIMENSION,
        max_array_layers: MAX_ARRAY_LAYERS,
        max_samples: MAX_SAMPLES,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GpuAllocationError {
    InvalidEnum,
    InvalidValue,
    InvalidOperation,
    OutOfMemory,
}

#[derive(Debug)]
struct ProcessUsage {
    max_bytes: u64,
    bytes: AtomicU64,
}

impl ProcessUsage {
    fn new(max_bytes: u64) -> Self {
        Self {
            max_bytes,
            bytes: AtomicU64::new(0),
        }
    }

    fn try_grow(self: &Arc<Self>, bytes: u64) -> Result<ProcessGrowth, GpuAllocationError> {
        if bytes == 0 {
            return Ok(ProcessGrowth {
                usage: Arc::clone(self),
                bytes: 0,
                armed: false,
            });
        }
        self.bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|next| *next <= self.max_bytes)
            })
            .map_err(|_| GpuAllocationError::OutOfMemory)?;
        Ok(ProcessGrowth {
            usage: Arc::clone(self),
            bytes,
            armed: true,
        })
    }

    fn release(&self, bytes: u64) {
        if bytes != 0 {
            let previous = self.bytes.fetch_sub(bytes, Ordering::AcqRel);
            debug_assert!(previous >= bytes, "WebGL process GPU accounting underflow");
        }
    }

    #[cfg(test)]
    fn snapshot(&self) -> u64 {
        self.bytes.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
struct ProcessGrowth {
    usage: Arc<ProcessUsage>,
    bytes: u64,
    armed: bool,
}

impl ProcessGrowth {
    fn commit(&mut self) {
        self.armed = false;
    }
}

impl Drop for ProcessGrowth {
    fn drop(&mut self) {
        if self.armed {
            self.usage.release(self.bytes);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct TextureSubresource {
    target: u32,
    level: u32,
}

#[derive(Debug)]
enum TextureStorage {
    Mutable(HashMap<TextureSubresource, u64>),
    Immutable(u64),
}

impl TextureStorage {
    fn byte_len(&self) -> u64 {
        match self {
            Self::Mutable(images) => images.values().copied().sum(),
            Self::Immutable(bytes) => *bytes,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SubresourceInfo {
    width: u32,
    height: u32,
    bytes_per_texel: Option<u64>,
}

#[derive(Debug)]
struct TextureRecord {
    owner: CanvasId,
    storage: TextureStorage,
}

#[derive(Debug)]
struct RenderbufferRecord {
    owner: CanvasId,
    bytes: u64,
}

/// The four texture binding targets a WebGL 2 context can bind to, in slot
/// order. Fixed by [`normalize_binding_target`], which rejects everything else
/// before it can reach the ledger.
const BINDING_TARGETS: [u32; 4] = [
    GL_TEXTURE_2D,
    GL_TEXTURE_CUBE_MAP,
    GL_TEXTURE_3D,
    GL_TEXTURE_2D_ARRAY,
];

/// Which texture is bound to each `(unit, target)` pair of one context.
///
/// **The key space here is completely dense and completely bounded**, and was
/// nonetheless a `HashMap<(u32, u32), Option<TextureId>>`: `unit` is validated
/// into `GL_TEXTURE0 .. GL_TEXTURE0 + WEBGL_TEXTURE_UNIT_COUNT` by
/// [`is_valid_texture_unit`] and `target` into one of exactly four values by
/// [`normalize_binding_target`], both *before* reaching this ledger. So every
/// `bindTexture` hashed a pair of `u32`s to address one of 128 slots that could
/// have been indexed directly — and unlike the dedup shadows this is a ledger,
/// so the write happens on every call and cannot be skipped.
///
/// A flat array is also exactly equivalent, not merely close: the one reader,
/// [`WebGlGpuBudget::bound_texture`], finishes with `.copied().flatten()`, so an
/// absent key and a key holding `None` were already indistinguishable to it.
/// That is what lets `None` cover both "never bound" and "explicitly unbound"
/// with no tri-state.
///
/// 1 KiB per context, against 1-4 contexts.
#[derive(Debug)]
struct TextureUnitLedger {
    /// `slots[unit_index * BINDING_TARGETS.len() + target_index]`.
    slots: [Option<TextureId>; (WEBGL_TEXTURE_UNIT_COUNT as usize) * BINDING_TARGETS.len()],
}

impl Default for TextureUnitLedger {
    fn default() -> Self {
        Self {
            slots: [None; (WEBGL_TEXTURE_UNIT_COUNT as usize) * BINDING_TARGETS.len()],
        }
    }
}

impl TextureUnitLedger {
    /// Slot for a `(unit, target)` pair that has already passed validation.
    /// `None` for anything else, which forwards the caller to its existing
    /// "not a valid binding" path rather than inventing a slot.
    #[inline]
    fn slot(unit: u32, target: u32) -> Option<usize> {
        let unit_index = unit.wrapping_sub(GL_TEXTURE0) as usize;
        if unit_index >= WEBGL_TEXTURE_UNIT_COUNT as usize {
            return None;
        }
        let target_index = BINDING_TARGETS.iter().position(|t| *t == target)?;
        Some(unit_index * BINDING_TARGETS.len() + target_index)
    }

    #[inline]
    fn set(&mut self, unit: u32, target: u32, texture: Option<TextureId>) {
        if let Some(i) = Self::slot(unit, target) {
            self.slots[i] = texture;
        }
    }

    #[inline]
    fn get(&self, unit: u32, target: u32) -> Option<TextureId> {
        Self::slot(unit, target).and_then(|i| self.slots[i])
    }

    /// Drop every binding that names `texture`.
    ///
    /// Deleting a texture unbinds it from every unit of every context
    /// (GLES 3.0 §3.8.14), so a ledger still naming it would charge a later
    /// upload against a texture that no longer exists.
    #[inline]
    fn forget_texture(&mut self, texture: TextureId) {
        for slot in &mut self.slots {
            if *slot == Some(texture) {
                *slot = None;
            }
        }
    }
}

#[derive(Debug)]
struct BindingState {
    active_texture: u32,
    textures: TextureUnitLedger,
    renderbuffer: Option<RenderbufferId>,
}

impl Default for BindingState {
    fn default() -> Self {
        Self {
            active_texture: GL_TEXTURE0,
            textures: TextureUnitLedger::default(),
            renderbuffer: None,
        }
    }
}

#[derive(Debug)]
enum PreparedKind {
    TextureImage {
        texture: TextureId,
        subresource: TextureSubresource,
        bytes: u64,
        info: SubresourceInfo,
    },
    TextureImmutable {
        texture: TextureId,
        bytes: u64,
    },
    GenerateMipmap {
        texture: TextureId,
        generated: Vec<(TextureSubresource, u64, SubresourceInfo)>,
    },
    Renderbuffer {
        renderbuffer: RenderbufferId,
        bytes: u64,
    },
}

/// Admission token held across the driver allocation call.
///
/// Dropping it before commit rolls back process growth. Commit performs no
/// allocation: mutable texture maps reserve their slot during preparation.
#[derive(Debug)]
pub(crate) struct PreparedGpuAllocation {
    canvas_id: CanvasId,
    old_object_bytes: u64,
    new_object_bytes: u64,
    #[cfg(test)]
    allocation_bytes: u64,
    kind: PreparedKind,
    process_growth: ProcessGrowth,
}

impl PreparedGpuAllocation {
    #[cfg(test)]
    fn byte_len(&self) -> u64 {
        self.allocation_bytes
    }
}

#[derive(Debug)]
pub(crate) struct WebGlGpuBudget {
    limits: GpuBudgetLimits,
    process: Arc<ProcessUsage>,
    contexts: HashMap<CanvasId, u64>,
    /// See [`crate::canvas_keyed`]: reached on every `bindTexture` /
    /// `activeTexture`, one entry per live context.
    bindings: crate::canvas_keyed::CanvasKeyed<BindingState>,
    textures: HashMap<TextureId, TextureRecord>,
    renderbuffers: HashMap<RenderbufferId, RenderbufferRecord>,
    subresource_info: HashMap<(TextureId, TextureSubresource), SubresourceInfo>,
}

/// Host-visible accounting domains.  WebGL storage is authoritative here;
/// DrawingBuffer, PBO/staging, Skia, and ImageStore have separate owners and
/// must publish their own values before a process-wide total can be reported.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct GpuDomainStats {
    pub(crate) webgl_context_bytes: u64,
    pub(crate) webgl_process_bytes: u64,
}

impl Default for WebGlGpuBudget {
    fn default() -> Self {
        Self::new()
    }
}

impl WebGlGpuBudget {
    pub(crate) fn new() -> Self {
        static PROCESS: OnceLock<Arc<ProcessUsage>> = OnceLock::new();
        let limits = GpuBudgetLimits::PRODUCTION;
        let process = Arc::clone(
            PROCESS.get_or_init(|| Arc::new(ProcessUsage::new(limits.max_process_bytes))),
        );
        Self::with_parts(limits, process)
    }
    fn with_parts(limits: GpuBudgetLimits, process: Arc<ProcessUsage>) -> Self {
        Self {
            limits,
            process,
            contexts: HashMap::new(),
            bindings: crate::canvas_keyed::CanvasKeyed::default(),
            textures: HashMap::new(),
            renderbuffers: HashMap::new(),
            subresource_info: HashMap::new(),
        }
    }

    pub(crate) fn create_texture(
        &mut self,
        canvas_id: CanvasId,
        texture: TextureId,
    ) -> Result<(), GpuAllocationError> {
        if texture == 0 || self.textures.contains_key(&texture) {
            return Err(GpuAllocationError::InvalidOperation);
        }
        self.textures
            .try_reserve(1)
            .map_err(|_| GpuAllocationError::OutOfMemory)?;
        self.bindings
            .try_reserve(1)
            .map_err(|_| GpuAllocationError::OutOfMemory)?;
        self.contexts
            .try_reserve(1)
            .map_err(|_| GpuAllocationError::OutOfMemory)?;
        self.contexts.entry(canvas_id).or_insert(0);
        self.bindings.entry(canvas_id).or_default();
        self.textures.insert(
            texture,
            TextureRecord {
                owner: canvas_id,
                storage: TextureStorage::Mutable(HashMap::new()),
            },
        );
        Ok(())
    }

    pub(crate) fn create_renderbuffer(
        &mut self,
        canvas_id: CanvasId,
        renderbuffer: RenderbufferId,
    ) -> Result<(), GpuAllocationError> {
        if renderbuffer == 0 || self.renderbuffers.contains_key(&renderbuffer) {
            return Err(GpuAllocationError::InvalidOperation);
        }
        self.renderbuffers
            .try_reserve(1)
            .map_err(|_| GpuAllocationError::OutOfMemory)?;
        self.bindings
            .try_reserve(1)
            .map_err(|_| GpuAllocationError::OutOfMemory)?;
        self.contexts
            .try_reserve(1)
            .map_err(|_| GpuAllocationError::OutOfMemory)?;
        self.contexts.entry(canvas_id).or_insert(0);
        self.bindings.entry(canvas_id).or_default();
        self.renderbuffers.insert(
            renderbuffer,
            RenderbufferRecord {
                owner: canvas_id,
                bytes: 0,
            },
        );
        Ok(())
    }

    pub(crate) fn active_texture(&mut self, canvas_id: CanvasId, unit: u32) {
        if !is_valid_texture_unit(unit) {
            return;
        }
        self.bindings.entry(canvas_id).or_default().active_texture = unit;
    }

    pub(crate) fn bind_texture(
        &mut self,
        canvas_id: CanvasId,
        target: u32,
        texture: Option<TextureId>,
    ) {
        let Some(binding_target) = normalize_binding_target(target) else {
            return;
        };
        let state = self.bindings.entry(canvas_id).or_default();
        let unit = state.active_texture;
        state.textures.set(unit, binding_target, texture);
    }

    pub(crate) fn bind_renderbuffer(
        &mut self,
        canvas_id: CanvasId,
        target: u32,
        renderbuffer: Option<RenderbufferId>,
    ) {
        if target == GL_RENDERBUFFER {
            self.bindings.entry(canvas_id).or_default().renderbuffer = renderbuffer;
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare_tex_image_2d(
        &mut self,
        canvas_id: CanvasId,
        target: u32,
        level: i32,
        internal_format: i32,
        width: i32,
        height: i32,
        border: i32,
        format: u32,
        ty: u32,
    ) -> Result<PreparedGpuAllocation, GpuAllocationError> {
        let (width, height, level) = validate_image_2d_dimensions(
            target,
            level,
            width,
            height,
            border,
            self.limits.max_2d_dimension,
        )?;
        let bpp = tex_image_bytes_per_pixel(internal_format, format, ty)?;
        let bytes = checked_texel_bytes(&[width, height], bpp)?;
        let texture = self.bound_texture(canvas_id, target)?;
        let subresource = TextureSubresource { target, level };
        let record = self
            .textures
            .get_mut(&texture)
            .ok_or(GpuAllocationError::InvalidOperation)?;
        if record.owner != canvas_id {
            return Err(GpuAllocationError::InvalidOperation);
        }
        let TextureStorage::Mutable(images) = &mut record.storage else {
            return Err(GpuAllocationError::InvalidOperation);
        };
        if !images.contains_key(&subresource) {
            images
                .try_reserve(1)
                .map_err(|_| GpuAllocationError::OutOfMemory)?;
        }
        let old_object_bytes = images
            .values()
            .try_fold(0u64, |total, value| total.checked_add(*value))
            .ok_or(GpuAllocationError::OutOfMemory)?;
        let old_subresource_bytes = images.get(&subresource).copied().unwrap_or(0);
        let new_object_bytes = old_object_bytes
            .checked_sub(old_subresource_bytes)
            .and_then(|base| base.checked_add(bytes))
            .ok_or(GpuAllocationError::OutOfMemory)?;
        self.prepare_transition(
            canvas_id,
            old_object_bytes,
            new_object_bytes,
            bytes,
            PreparedKind::TextureImage {
                texture,
                subresource,
                bytes,
                info: SubresourceInfo {
                    width,
                    height,
                    bytes_per_texel: Some(bpp),
                },
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare_compressed_tex_image_2d(
        &mut self,
        canvas_id: CanvasId,
        target: u32,
        level: i32,
        _internal_format: u32,
        width: i32,
        height: i32,
        border: i32,
        image_size: u32,
    ) -> Result<PreparedGpuAllocation, GpuAllocationError> {
        let (width, height, level) = validate_image_2d_dimensions(
            target,
            level,
            width,
            height,
            border,
            self.limits.max_2d_dimension,
        )?;
        let texture = self.bound_texture(canvas_id, target)?;
        let subresource = TextureSubresource { target, level };
        let record = self
            .textures
            .get_mut(&texture)
            .ok_or(GpuAllocationError::InvalidOperation)?;
        if record.owner != canvas_id {
            return Err(GpuAllocationError::InvalidOperation);
        }
        let TextureStorage::Mutable(images) = &mut record.storage else {
            return Err(GpuAllocationError::InvalidOperation);
        };
        let old_object_bytes = images
            .values()
            .try_fold(0u64, |sum, value| sum.checked_add(*value))
            .ok_or(GpuAllocationError::OutOfMemory)?;
        let old_subresource_bytes = images.get(&subresource).copied().unwrap_or(0);
        let bytes = u64::from(image_size);
        let new_object_bytes = old_object_bytes
            .checked_sub(old_subresource_bytes)
            .and_then(|base| base.checked_add(bytes))
            .ok_or(GpuAllocationError::OutOfMemory)?;
        self.prepare_transition(
            canvas_id,
            old_object_bytes,
            new_object_bytes,
            bytes,
            PreparedKind::TextureImage {
                texture,
                subresource,
                bytes,
                info: SubresourceInfo {
                    width,
                    height,
                    bytes_per_texel: None,
                },
            },
        )
    }

    pub(crate) fn prepare_generate_mipmap(
        &mut self,
        canvas_id: CanvasId,
        target: u32,
    ) -> Result<PreparedGpuAllocation, GpuAllocationError> {
        if target != GL_TEXTURE_2D && target != GL_TEXTURE_CUBE_MAP {
            return Err(GpuAllocationError::InvalidEnum);
        }
        let texture = self.bound_texture(canvas_id, target)?;
        let record = self
            .textures
            .get(&texture)
            .ok_or(GpuAllocationError::InvalidOperation)?;
        if record.owner != canvas_id {
            return Err(GpuAllocationError::InvalidOperation);
        }
        let TextureStorage::Mutable(images) = &record.storage else {
            return Err(GpuAllocationError::InvalidOperation);
        };
        let faces: Vec<u32> = if target == GL_TEXTURE_CUBE_MAP {
            (GL_TEXTURE_CUBE_MAP_POSITIVE_X..=GL_TEXTURE_CUBE_MAP_NEGATIVE_Z).collect()
        } else {
            vec![GL_TEXTURE_2D]
        };
        let mut generated = Vec::new();
        let old_object_bytes = images
            .values()
            .try_fold(0u64, |sum, value| sum.checked_add(*value))
            .ok_or(GpuAllocationError::OutOfMemory)?;
        let mut new_object_bytes = old_object_bytes;
        for face in faces {
            let base_key = TextureSubresource {
                target: face,
                level: 0,
            };
            let base_bytes = images
                .get(&base_key)
                .copied()
                .ok_or(GpuAllocationError::InvalidOperation)?;
            let info = self
                .subresource_info
                .get(&(texture, base_key))
                .copied()
                .ok_or(GpuAllocationError::InvalidOperation)?;
            let bpp = info
                .bytes_per_texel
                .ok_or(GpuAllocationError::InvalidOperation)?;
            if info.width == 0 || info.height == 0 {
                return Err(GpuAllocationError::InvalidOperation);
            }
            let expected = checked_texel_bytes(&[info.width, info.height], bpp)?;
            if expected != base_bytes {
                return Err(GpuAllocationError::InvalidOperation);
            }
            let levels = 32 - info.width.max(info.height).leading_zeros();
            let mut width = info.width;
            let mut height = info.height;
            for level in 1..levels {
                width = (width / 2).max(1);
                height = (height / 2).max(1);
                let key = TextureSubresource {
                    target: face,
                    level,
                };
                let bytes = checked_texel_bytes(&[width, height], bpp)?;
                let old = images.get(&key).copied().unwrap_or(0);
                new_object_bytes = new_object_bytes
                    .checked_sub(old)
                    .and_then(|sum| sum.checked_add(bytes))
                    .ok_or(GpuAllocationError::OutOfMemory)?;
                generated.push((
                    key,
                    bytes,
                    SubresourceInfo {
                        width,
                        height,
                        bytes_per_texel: Some(bpp),
                    },
                ));
            }
        }
        self.prepare_transition(
            canvas_id,
            old_object_bytes,
            new_object_bytes,
            new_object_bytes.saturating_sub(old_object_bytes),
            PreparedKind::GenerateMipmap { texture, generated },
        )
    }

    pub(crate) fn prepare_tex_storage_2d(
        &mut self,
        canvas_id: CanvasId,
        target: u32,
        levels: i32,
        internal_format: u32,
        width: i32,
        height: i32,
    ) -> Result<PreparedGpuAllocation, GpuAllocationError> {
        if target != GL_TEXTURE_2D && target != GL_TEXTURE_CUBE_MAP {
            return Err(GpuAllocationError::InvalidEnum);
        }
        let (width, height, levels) =
            validate_storage_dimensions(levels, width, height, self.limits.max_2d_dimension)?;
        let bpp =
            sized_internal_format_bytes(internal_format).ok_or(GpuAllocationError::InvalidEnum)?;
        let faces = if target == GL_TEXTURE_CUBE_MAP { 6 } else { 1 };
        let bytes = checked_mip_chain_bytes(width, height, 1, levels, false, bpp)?
            .checked_mul(faces)
            .ok_or(GpuAllocationError::OutOfMemory)?;
        let texture = self.bound_texture(canvas_id, target)?;
        let record = self
            .textures
            .get(&texture)
            .ok_or(GpuAllocationError::InvalidOperation)?;
        if record.owner != canvas_id || matches!(record.storage, TextureStorage::Immutable(_)) {
            return Err(GpuAllocationError::InvalidOperation);
        }
        let old_object_bytes = record.storage.byte_len();
        self.prepare_transition(
            canvas_id,
            old_object_bytes,
            bytes,
            bytes,
            PreparedKind::TextureImmutable { texture, bytes },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare_tex_image_3d(
        &mut self,
        canvas_id: CanvasId,
        target: u32,
        level: i32,
        internal_format: i32,
        width: i32,
        height: i32,
        depth: i32,
        border: i32,
        format: u32,
        ty: u32,
    ) -> Result<PreparedGpuAllocation, GpuAllocationError> {
        let (width, height, depth, level) =
            validate_image_3d_dimensions(target, level, width, height, depth, border, self.limits)?;
        let bpp = tex_image_bytes_per_pixel(internal_format, format, ty)?;
        let bytes = checked_texel_bytes(&[width, height, depth], bpp)?;
        let texture = self.bound_texture(canvas_id, target)?;
        let subresource = TextureSubresource { target, level };
        let record = self
            .textures
            .get_mut(&texture)
            .ok_or(GpuAllocationError::InvalidOperation)?;
        if record.owner != canvas_id {
            return Err(GpuAllocationError::InvalidOperation);
        }
        let TextureStorage::Mutable(images) = &mut record.storage else {
            return Err(GpuAllocationError::InvalidOperation);
        };
        if !images.contains_key(&subresource) {
            images
                .try_reserve(1)
                .map_err(|_| GpuAllocationError::OutOfMemory)?;
        }
        let old_object_bytes = images
            .values()
            .try_fold(0u64, |total, value| total.checked_add(*value))
            .ok_or(GpuAllocationError::OutOfMemory)?;
        let old_subresource_bytes = images.get(&subresource).copied().unwrap_or(0);
        let new_object_bytes = old_object_bytes
            .checked_sub(old_subresource_bytes)
            .and_then(|base| base.checked_add(bytes))
            .ok_or(GpuAllocationError::OutOfMemory)?;
        self.prepare_transition(
            canvas_id,
            old_object_bytes,
            new_object_bytes,
            bytes,
            PreparedKind::TextureImage {
                texture,
                subresource,
                bytes,
                info: SubresourceInfo {
                    width,
                    height,
                    bytes_per_texel: Some(bpp),
                },
            },
        )
    }

    pub(crate) fn prepare_tex_storage_3d(
        &mut self,
        canvas_id: CanvasId,
        target: u32,
        levels: i32,
        internal_format: u32,
        width: i32,
        height: i32,
        depth: i32,
    ) -> Result<PreparedGpuAllocation, GpuAllocationError> {
        if target != GL_TEXTURE_3D && target != GL_TEXTURE_2D_ARRAY {
            return Err(GpuAllocationError::InvalidEnum);
        }
        let (width, height, depth, levels) =
            validate_storage_3d_dimensions(target, levels, width, height, depth, self.limits)?;
        let bpp =
            sized_internal_format_bytes(internal_format).ok_or(GpuAllocationError::InvalidEnum)?;
        let bytes =
            checked_mip_chain_bytes(width, height, depth, levels, target == GL_TEXTURE_3D, bpp)?;
        let texture = self.bound_texture(canvas_id, target)?;
        let record = self
            .textures
            .get(&texture)
            .ok_or(GpuAllocationError::InvalidOperation)?;
        if record.owner != canvas_id || matches!(record.storage, TextureStorage::Immutable(_)) {
            return Err(GpuAllocationError::InvalidOperation);
        }
        let old_object_bytes = record.storage.byte_len();
        self.prepare_transition(
            canvas_id,
            old_object_bytes,
            bytes,
            bytes,
            PreparedKind::TextureImmutable { texture, bytes },
        )
    }

    pub(crate) fn prepare_renderbuffer_storage(
        &mut self,
        canvas_id: CanvasId,
        target: u32,
        internal_format: u32,
        width: i32,
        height: i32,
        samples: i32,
    ) -> Result<PreparedGpuAllocation, GpuAllocationError> {
        if target != GL_RENDERBUFFER {
            return Err(GpuAllocationError::InvalidEnum);
        }
        if width < 0 || height < 0 || samples < 0 {
            return Err(GpuAllocationError::InvalidValue);
        }
        let width = width as u32;
        let height = height as u32;
        let samples = samples as u32;
        if width > self.limits.max_2d_dimension
            || height > self.limits.max_2d_dimension
            || samples > self.limits.max_samples
        {
            return Err(GpuAllocationError::InvalidValue);
        }
        let bpp =
            sized_internal_format_bytes(internal_format).ok_or(GpuAllocationError::InvalidEnum)?;
        let bytes = checked_texel_bytes(&[width, height, samples.max(1)], bpp)?;
        let renderbuffer = self
            .bindings
            .get(&canvas_id)
            .and_then(|state| state.renderbuffer)
            .ok_or(GpuAllocationError::InvalidOperation)?;
        let record = self
            .renderbuffers
            .get(&renderbuffer)
            .ok_or(GpuAllocationError::InvalidOperation)?;
        if record.owner != canvas_id {
            return Err(GpuAllocationError::InvalidOperation);
        }
        self.prepare_transition(
            canvas_id,
            record.bytes,
            bytes,
            bytes,
            PreparedKind::Renderbuffer {
                renderbuffer,
                bytes,
            },
        )
    }

    fn prepare_transition(
        &self,
        canvas_id: CanvasId,
        old_object_bytes: u64,
        new_object_bytes: u64,
        allocation_bytes: u64,
        kind: PreparedKind,
    ) -> Result<PreparedGpuAllocation, GpuAllocationError> {
        let context_bytes = self.contexts.get(&canvas_id).copied().unwrap_or(0);
        let projected = context_bytes
            .checked_sub(old_object_bytes)
            .and_then(|base| base.checked_add(new_object_bytes))
            .filter(|next| *next <= self.limits.max_context_bytes)
            .ok_or(GpuAllocationError::OutOfMemory)?;
        let _ = projected;
        let growth = new_object_bytes.saturating_sub(old_object_bytes);
        let process_growth = self.process.try_grow(growth)?;
        Ok(PreparedGpuAllocation {
            canvas_id,
            old_object_bytes,
            new_object_bytes,
            #[cfg(test)]
            allocation_bytes,
            kind,
            process_growth,
        })
    }

    pub(crate) fn commit(&mut self, mut prepared: PreparedGpuAllocation) {
        let context = self.contexts.entry(prepared.canvas_id).or_insert(0);
        *context = context
            .checked_sub(prepared.old_object_bytes)
            .and_then(|base| base.checked_add(prepared.new_object_bytes))
            .expect("prepared WebGL context accounting changed before commit");
        match prepared.kind {
            PreparedKind::TextureImage {
                texture,
                subresource,
                bytes,
                info,
            } => {
                let record = self
                    .textures
                    .get_mut(&texture)
                    .expect("prepared WebGL texture disappeared before commit");
                let TextureStorage::Mutable(images) = &mut record.storage else {
                    panic!("prepared mutable WebGL texture became immutable before commit");
                };
                images.insert(subresource, bytes);
                self.subresource_info.insert((texture, subresource), info);
            }
            PreparedKind::TextureImmutable { texture, bytes } => {
                let record = self
                    .textures
                    .get_mut(&texture)
                    .expect("prepared WebGL texture disappeared before commit");
                record.storage = TextureStorage::Immutable(bytes);
            }
            PreparedKind::GenerateMipmap { texture, generated } => {
                let record = self
                    .textures
                    .get_mut(&texture)
                    .expect("prepared WebGL texture disappeared before commit");
                let TextureStorage::Mutable(images) = &mut record.storage else {
                    panic!("prepared mutable WebGL texture became immutable before commit");
                };
                for (subresource, bytes, info) in generated {
                    images.insert(subresource, bytes);
                    self.subresource_info.insert((texture, subresource), info);
                }
            }
            PreparedKind::Renderbuffer {
                renderbuffer,
                bytes,
            } => {
                self.renderbuffers
                    .get_mut(&renderbuffer)
                    .expect("prepared WebGL renderbuffer disappeared before commit")
                    .bytes = bytes;
            }
        }
        if prepared.new_object_bytes < prepared.old_object_bytes {
            self.process
                .release(prepared.old_object_bytes - prepared.new_object_bytes);
        }
        prepared.process_growth.commit();
    }

    pub(crate) fn delete_texture(&mut self, texture: TextureId) -> u64 {
        let Some(record) = self.textures.remove(&texture) else {
            return 0;
        };
        for state in self.bindings.values_mut() {
            state.textures.forget_texture(texture);
        }
        self.subresource_info.retain(|(id, _), _| *id != texture);
        let bytes = record.storage.byte_len();
        self.release_context_bytes(record.owner, bytes);
        bytes
    }

    pub(crate) fn delete_renderbuffer(&mut self, renderbuffer: RenderbufferId) -> u64 {
        let Some(record) = self.renderbuffers.remove(&renderbuffer) else {
            return 0;
        };
        for state in self.bindings.values_mut() {
            if state.renderbuffer == Some(renderbuffer) {
                state.renderbuffer = None;
            }
        }
        self.release_context_bytes(record.owner, record.bytes);
        record.bytes
    }

    fn release_context_bytes(&mut self, canvas_id: CanvasId, bytes: u64) {
        if bytes == 0 {
            return;
        }
        let context = self
            .contexts
            .get_mut(&canvas_id)
            .expect("live WebGL allocation has context accounting");
        *context = context
            .checked_sub(bytes)
            .expect("WebGL context GPU accounting underflow");
        self.process.release(bytes);
    }

    pub(crate) fn release_context(&mut self, canvas_id: CanvasId) -> u64 {
        self.bindings.remove(&canvas_id);
        let textures: Vec<TextureId> = self
            .textures
            .iter()
            .filter_map(|(id, record)| (record.owner == canvas_id).then_some(*id))
            .collect();
        for texture in textures {
            self.subresource_info.retain(|(id, _), _| *id != texture);
        }
        self.textures.retain(|_, record| record.owner != canvas_id);
        self.renderbuffers
            .retain(|_, record| record.owner != canvas_id);
        let bytes = self.contexts.remove(&canvas_id).unwrap_or(0);
        self.process.release(bytes);
        bytes
    }

    /// Snapshot WebGL's own domain counters.  Other GPU domains are
    /// intentionally not folded in: their owners can publish independently.
    pub(crate) fn domain_stats(&self) -> GpuDomainStats {
        GpuDomainStats {
            webgl_context_bytes: self.contexts.values().copied().sum(),
            webgl_process_bytes: self.process.bytes.load(Ordering::Acquire),
        }
    }

    pub(crate) fn clear(&mut self) -> u64 {
        self.bindings.clear();
        self.textures.clear();
        self.renderbuffers.clear();
        self.subresource_info.clear();
        let bytes = self.contexts.values().copied().sum();
        self.contexts.clear();
        self.process.release(bytes);
        bytes
    }

    fn bound_texture(
        &self,
        canvas_id: CanvasId,
        target: u32,
    ) -> Result<TextureId, GpuAllocationError> {
        let binding_target =
            normalize_image_binding_target(target).ok_or(GpuAllocationError::InvalidEnum)?;
        let state = self
            .bindings
            .get(&canvas_id)
            .ok_or(GpuAllocationError::InvalidOperation)?;
        state
            .textures
            .get(state.active_texture, binding_target)
            .ok_or(GpuAllocationError::InvalidOperation)
    }

    #[cfg(test)]
    fn context_usage(&self, canvas_id: CanvasId) -> u64 {
        self.contexts.get(&canvas_id).copied().unwrap_or(0)
    }
}

impl Drop for WebGlGpuBudget {
    fn drop(&mut self) {
        self.clear();
    }
}

fn normalize_binding_target(target: u32) -> Option<u32> {
    match target {
        GL_TEXTURE_2D | GL_TEXTURE_CUBE_MAP | GL_TEXTURE_3D | GL_TEXTURE_2D_ARRAY => Some(target),
        _ => None,
    }
}

pub(crate) fn is_valid_texture_unit(unit: u32) -> bool {
    (GL_TEXTURE0..GL_TEXTURE0 + WEBGL_TEXTURE_UNIT_COUNT).contains(&unit)
}

fn normalize_image_binding_target(target: u32) -> Option<u32> {
    match target {
        GL_TEXTURE_CUBE_MAP_POSITIVE_X..=GL_TEXTURE_CUBE_MAP_NEGATIVE_Z => {
            Some(GL_TEXTURE_CUBE_MAP)
        }
        _ => normalize_binding_target(target),
    }
}

fn validate_image_2d_dimensions(
    target: u32,
    level: i32,
    width: i32,
    height: i32,
    border: i32,
    max_dimension: u32,
) -> Result<(u32, u32, u32), GpuAllocationError> {
    if target != GL_TEXTURE_2D
        && !(GL_TEXTURE_CUBE_MAP_POSITIVE_X..=GL_TEXTURE_CUBE_MAP_NEGATIVE_Z).contains(&target)
    {
        return Err(GpuAllocationError::InvalidEnum);
    }
    if level < 0 || width < 0 || height < 0 || border != 0 {
        return Err(GpuAllocationError::InvalidValue);
    }
    let level = level as u32;
    if level >= 32 - max_dimension.leading_zeros() {
        return Err(GpuAllocationError::InvalidValue);
    }
    let max_at_level = max_dimension.checked_shr(level).unwrap_or(0).max(1);
    let width = width as u32;
    let height = height as u32;
    if width > max_at_level || height > max_at_level {
        return Err(GpuAllocationError::InvalidValue);
    }
    if target != GL_TEXTURE_2D && width != height {
        return Err(GpuAllocationError::InvalidValue);
    }
    Ok((width, height, level))
}

fn validate_storage_dimensions(
    levels: i32,
    width: i32,
    height: i32,
    max_dimension: u32,
) -> Result<(u32, u32, u32), GpuAllocationError> {
    if levels <= 0 || width <= 0 || height <= 0 {
        return Err(GpuAllocationError::InvalidValue);
    }
    let width = width as u32;
    let height = height as u32;
    let levels = levels as u32;
    if width > max_dimension || height > max_dimension {
        return Err(GpuAllocationError::InvalidValue);
    }
    let max_levels = 32 - width.max(height).leading_zeros();
    if levels > max_levels {
        return Err(GpuAllocationError::InvalidValue);
    }
    Ok((width, height, levels))
}

fn validate_image_3d_dimensions(
    target: u32,
    level: i32,
    width: i32,
    height: i32,
    depth: i32,
    border: i32,
    limits: GpuBudgetLimits,
) -> Result<(u32, u32, u32, u32), GpuAllocationError> {
    if target != GL_TEXTURE_3D && target != GL_TEXTURE_2D_ARRAY {
        return Err(GpuAllocationError::InvalidEnum);
    }
    if level < 0 || width < 0 || height < 0 || depth < 0 || border != 0 {
        return Err(GpuAllocationError::InvalidValue);
    }
    let level = level as u32;
    let base_max = if target == GL_TEXTURE_3D {
        limits.max_3d_dimension
    } else {
        limits.max_2d_dimension
    };
    if level >= 32 - base_max.leading_zeros() {
        return Err(GpuAllocationError::InvalidValue);
    }
    let max_at_level = base_max.checked_shr(level).unwrap_or(0).max(1);
    let width = width as u32;
    let height = height as u32;
    let depth = depth as u32;
    let max_depth = if target == GL_TEXTURE_3D {
        limits
            .max_3d_dimension
            .checked_shr(level)
            .unwrap_or(0)
            .max(1)
    } else {
        limits.max_array_layers
    };
    if width > max_at_level || height > max_at_level || depth > max_depth {
        return Err(GpuAllocationError::InvalidValue);
    }
    Ok((width, height, depth, level))
}

fn validate_storage_3d_dimensions(
    target: u32,
    levels: i32,
    width: i32,
    height: i32,
    depth: i32,
    limits: GpuBudgetLimits,
) -> Result<(u32, u32, u32, u32), GpuAllocationError> {
    if levels <= 0 || width <= 0 || height <= 0 || depth <= 0 {
        return Err(GpuAllocationError::InvalidValue);
    }
    let width = width as u32;
    let height = height as u32;
    let depth = depth as u32;
    let levels = levels as u32;
    let (max_xy, max_depth, mip_basis) = if target == GL_TEXTURE_3D {
        (
            limits.max_3d_dimension,
            limits.max_3d_dimension,
            width.max(height).max(depth),
        )
    } else {
        (
            limits.max_2d_dimension,
            limits.max_array_layers,
            width.max(height),
        )
    };
    if width > max_xy || height > max_xy || depth > max_depth {
        return Err(GpuAllocationError::InvalidValue);
    }
    let max_levels = 32 - mip_basis.leading_zeros();
    if levels > max_levels {
        return Err(GpuAllocationError::InvalidValue);
    }
    Ok((width, height, depth, levels))
}

fn checked_texel_bytes(dimensions: &[u32], bpp: u64) -> Result<u64, GpuAllocationError> {
    dimensions
        .iter()
        .try_fold(bpp, |bytes, dimension| {
            bytes.checked_mul(u64::from(*dimension))
        })
        .ok_or(GpuAllocationError::OutOfMemory)
}

fn checked_mip_chain_bytes(
    mut width: u32,
    mut height: u32,
    mut depth: u32,
    levels: u32,
    mip_depth: bool,
    bpp: u64,
) -> Result<u64, GpuAllocationError> {
    let mut total = 0u64;
    for _ in 0..levels {
        let level = checked_texel_bytes(&[width, height, depth], bpp)?;
        total = total
            .checked_add(level)
            .ok_or(GpuAllocationError::OutOfMemory)?;
        width = (width / 2).max(1);
        height = (height / 2).max(1);
        if mip_depth {
            depth = (depth / 2).max(1);
        }
    }
    Ok(total)
}

fn tex_image_bytes_per_pixel(
    internal_format: i32,
    format: u32,
    ty: u32,
) -> Result<u64, GpuAllocationError> {
    let internal_format =
        u32::try_from(internal_format).map_err(|_| GpuAllocationError::InvalidEnum)?;
    if let Some(bytes) = sized_internal_format_bytes(internal_format) {
        return Ok(bytes);
    }
    if internal_format != format {
        return Err(GpuAllocationError::InvalidOperation);
    }
    upload_format_bytes(format, ty).ok_or(GpuAllocationError::InvalidEnum)
}

fn upload_format_bytes(format: u32, ty: u32) -> Option<u64> {
    match ty {
        0x8363 if format == 0x1907 => Some(2), // UNSIGNED_SHORT_5_6_5 / RGB
        0x8033 | 0x8034 if format == 0x1908 => Some(2), // 4_4_4_4 / 5_5_5_1
        0x8368 if format == 0x1908 => Some(4), // 2_10_10_10_REV
        0x84FA if format == 0x84F9 => Some(4), // UNSIGNED_INT_24_8 / DEPTH_STENCIL
        0x8DAD if format == 0x84F9 => Some(8), // FLOAT_32_UNSIGNED_INT_24_8_REV
        0x1401 | 0x1400 => {
            format_components(format).map(|components| if components == 3 { 4 } else { components })
        }
        0x1403 | 0x1402 | 0x140B | 0x8D61 => format_components(format).map(|components| {
            let raw = components * 2;
            if components == 3 { 8 } else { raw }
        }),
        0x1405 | 0x1404 | 0x1406 => format_components(format).map(|components| {
            let raw = components * 4;
            if components == 3 { 16 } else { raw }
        }),
        _ => None,
    }
}

fn format_components(format: u32) -> Option<u64> {
    match format {
        0x1906 | 0x1909 | 0x1902 | 0x1903 | 0x8D94 => Some(1), // A/L/D/R/R_INTEGER
        0x190A | 0x8227 | 0x8228 => Some(2),                   // LA/RG/RG_INTEGER
        0x1907 | 0x8C40 | 0x8D98 => Some(3),                   // RGB/SRGB/RGB_INTEGER
        0x1908 | 0x8C42 | 0x8D99 => Some(4),                   // RGBA/SRGB_ALPHA/RGBA_INTEGER
        0x84F9 => Some(1),
        _ => None,
    }
}

fn sized_internal_format_bytes(format: u32) -> Option<u64> {
    match format {
        // 8-bit single channel / stencil.
        glow::R8 | glow::R8_SNORM | glow::R8I | glow::R8UI | glow::STENCIL_INDEX8 => Some(1),
        // 16-bit single, 8-bit dual, or packed 16-bit color/depth.
        glow::R16F
        | glow::R16I
        | glow::R16UI
        | glow::RG8
        | glow::RG8_SNORM
        | glow::RG8I
        | glow::RG8UI
        | glow::RGB565
        | glow::RGBA4
        | glow::RGB5_A1
        | glow::DEPTH_COMPONENT16 => Some(2),
        // 32-bit single, 16-bit dual, 8-bit RGB/RGBA (RGB conservatively
        // rounded to four bytes), packed 32-bit color/depth.
        glow::R32F
        | glow::R32I
        | glow::R32UI
        | glow::RG16F
        | glow::RG16I
        | glow::RG16UI
        | glow::RGB8
        | glow::RGB8_SNORM
        | glow::SRGB8
        | glow::RGBA8
        | glow::RGBA8_SNORM
        | glow::SRGB8_ALPHA8
        | glow::R11F_G11F_B10F
        | glow::RGB9_E5
        | glow::RGB8I
        | glow::RGB8UI
        | glow::RGBA8I
        | glow::RGBA8UI
        | glow::RGB10_A2
        | glow::RGB10_A2UI
        | glow::DEPTH_COMPONENT24
        | glow::DEPTH_COMPONENT32F
        | glow::DEPTH24_STENCIL8
        | glow::DEPTH_STENCIL => Some(4),
        // 64-bit dual/RGB16F/RGBA16F and DEPTH32F_STENCIL8.
        glow::RG32F
        | glow::RG32I
        | glow::RG32UI
        | glow::RGB16F
        | glow::RGB16I
        | glow::RGB16UI
        | glow::RGBA16F
        | glow::RGBA16I
        | glow::RGBA16UI
        | glow::DEPTH32F_STENCIL8 => Some(8),
        // RGB32F is conservatively aligned to 16 bytes; RGBA32F is 16.
        glow::RGB32F
        | glow::RGB32I
        | glow::RGB32UI
        | glow::RGBA32F
        | glow::RGBA32I
        | glow::RGBA32UI => Some(16),
        _ => None,
    }
}

#[cfg(test)]
struct GpuBudgetTestScope {
    limits: GpuBudgetLimits,
    process: Arc<ProcessUsage>,
}

#[cfg(test)]
impl GpuBudgetTestScope {
    fn new(limits: GpuBudgetLimits) -> Self {
        Self {
            process: Arc::new(ProcessUsage::new(limits.max_process_bytes)),
            limits,
        }
    }

    fn registry(&self) -> WebGlGpuBudget {
        WebGlGpuBudget::with_parts(self.limits, Arc::clone(&self.process))
    }

    fn process_usage(&self) -> u64 {
        self.process.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::{GpuAllocationError, GpuBudgetLimits, GpuBudgetTestScope};
    use std::collections::HashMap;

    const TEXTURE_2D: u32 = 0x0DE1;
    const TEXTURE_CUBE_MAP: u32 = 0x8513;
    const TEXTURE_CUBE_MAP_POSITIVE_X: u32 = 0x8515;
    const TEXTURE_3D: u32 = 0x806F;
    const TEXTURE_2D_ARRAY: u32 = 0x8C1A;
    const TEXTURE0: u32 = 0x84C0;
    const RENDERBUFFER: u32 = 0x8D41;
    const RGBA: u32 = 0x1908;
    const UNSIGNED_BYTE: u32 = 0x1401;
    const RGBA8: u32 = 0x8058;

    #[derive(Default)]
    struct RecordingGl {
        storage: HashMap<(u32, u32), u64>,
    }

    impl RecordingGl {
        fn define(&mut self, texture: u32, level: u32, bytes: u64) {
            self.storage.insert((texture, level), bytes);
        }

        fn bytes(&self) -> u64 {
            self.storage.values().copied().sum()
        }
    }

    fn limits(context_bytes: u64, process_bytes: u64) -> GpuBudgetLimits {
        GpuBudgetLimits {
            max_context_bytes: context_bytes,
            max_process_bytes: process_bytes,
            max_2d_dimension: 16_384,
            max_3d_dimension: 2_048,
            max_array_layers: 2_048,
            max_samples: 16,
        }
    }

    #[test]
    fn checked_estimators_cover_mips_cube_depth_array_and_samples() {
        let scope = GpuBudgetTestScope::new(limits(16 * 1024, 32 * 1024));
        let mut budget = scope.registry();

        budget.create_texture(1, 11).unwrap();
        budget.bind_texture(1, TEXTURE_2D, Some(11));
        let image = budget
            .prepare_tex_image_2d(1, TEXTURE_2D, 0, RGBA as i32, 8, 4, 0, RGBA, UNSIGNED_BYTE)
            .unwrap();
        assert_eq!(image.byte_len(), 128);
        budget.commit(image);

        budget.create_texture(1, 12).unwrap();
        budget.bind_texture(1, TEXTURE_CUBE_MAP, Some(12));
        let cube = budget
            .prepare_tex_storage_2d(1, TEXTURE_CUBE_MAP, 4, RGBA8, 8, 4)
            .unwrap();
        assert_eq!(cube.byte_len(), 172 * 6);
        budget.commit(cube);

        budget.create_texture(1, 13).unwrap();
        budget.bind_texture(1, TEXTURE_3D, Some(13));
        let volume = budget
            .prepare_tex_storage_3d(1, TEXTURE_3D, 4, RGBA8, 8, 4, 2)
            .unwrap();
        assert_eq!(volume.byte_len(), 300);
        budget.commit(volume);

        budget.create_texture(1, 14).unwrap();
        budget.bind_texture(1, TEXTURE_2D_ARRAY, Some(14));
        let array = budget
            .prepare_tex_storage_3d(1, TEXTURE_2D_ARRAY, 4, RGBA8, 8, 4, 3)
            .unwrap();
        assert_eq!(array.byte_len(), 516);
        budget.commit(array);

        budget.create_renderbuffer(1, 21).unwrap();
        budget.bind_renderbuffer(1, RENDERBUFFER, Some(21));
        let msaa = budget
            .prepare_renderbuffer_storage(1, RENDERBUFFER, RGBA8, 4, 4, 4)
            .unwrap();
        assert_eq!(msaa.byte_len(), 256);
        budget.commit(msaa);

        assert_eq!(budget.context_usage(1), 128 + 1_032 + 300 + 516 + 256);
        assert_eq!(scope.process_usage(), budget.context_usage(1));
    }

    #[test]
    fn invalid_dimensions_levels_targets_and_unknown_formats_fail_closed() {
        let scope = GpuBudgetTestScope::new(limits(1_024, 2_048));
        let mut budget = scope.registry();
        budget.create_texture(1, 1).unwrap();
        budget.bind_texture(1, TEXTURE_2D, Some(1));

        assert_eq!(
            budget
                .prepare_tex_image_2d(1, TEXTURE_2D, 0, RGBA as i32, -1, 1, 0, RGBA, UNSIGNED_BYTE,)
                .unwrap_err(),
            GpuAllocationError::InvalidValue
        );
        assert_eq!(
            budget
                .prepare_tex_image_2d(1, TEXTURE_2D, 15, RGBA as i32, 1, 1, 0, RGBA, UNSIGNED_BYTE,)
                .unwrap_err(),
            GpuAllocationError::InvalidValue
        );
        assert_eq!(
            budget
                .prepare_tex_storage_2d(1, TEXTURE_2D, 5, RGBA8, 8, 8)
                .unwrap_err(),
            GpuAllocationError::InvalidValue
        );
        assert_eq!(
            budget
                .prepare_tex_storage_2d(1, 0xDEAD, 1, RGBA8, 1, 1)
                .unwrap_err(),
            GpuAllocationError::InvalidEnum
        );
        assert_eq!(
            budget
                .prepare_tex_storage_2d(1, TEXTURE_2D, 1, 0xDEAD, 1, 1)
                .unwrap_err(),
            GpuAllocationError::InvalidEnum
        );
        assert_eq!(budget.context_usage(1), 0);
        assert_eq!(scope.process_usage(), 0);
    }

    #[test]
    fn redefine_replaces_usage_and_abandoned_growth_rolls_back() {
        let scope = GpuBudgetTestScope::new(limits(128, 256));
        let mut budget = scope.registry();
        budget.create_texture(1, 1).unwrap();
        budget.bind_texture(1, TEXTURE_2D, Some(1));

        let first = budget
            .prepare_tex_image_2d(1, TEXTURE_2D, 0, RGBA as i32, 4, 4, 0, RGBA, UNSIGNED_BYTE)
            .unwrap();
        budget.commit(first);
        assert_eq!(budget.context_usage(1), 64);
        assert_eq!(scope.process_usage(), 64);

        let same_size = budget
            .prepare_tex_image_2d(1, TEXTURE_2D, 0, RGBA as i32, 4, 4, 0, RGBA, UNSIGNED_BYTE)
            .unwrap();
        assert_eq!(
            scope.process_usage(),
            64,
            "replacement is not double-counted"
        );
        budget.commit(same_size);

        let growth = budget
            .prepare_tex_image_2d(1, TEXTURE_2D, 0, RGBA as i32, 8, 4, 0, RGBA, UNSIGNED_BYTE)
            .unwrap();
        assert_eq!(scope.process_usage(), 128, "growth is reserved before GL");
        drop(growth);
        assert_eq!(budget.context_usage(1), 64);
        assert_eq!(scope.process_usage(), 64);

        let shrink = budget
            .prepare_tex_image_2d(1, TEXTURE_2D, 0, RGBA as i32, 2, 2, 0, RGBA, UNSIGNED_BYTE)
            .unwrap();
        budget.commit(shrink);
        assert_eq!(budget.context_usage(1), 16);
        assert_eq!(scope.process_usage(), 16);
    }

    #[test]
    fn context_and_process_quotas_span_objects_and_registries() {
        let scope = GpuBudgetTestScope::new(limits(128, 192));
        let mut first = scope.registry();
        let mut second = scope.registry();

        for (budget, canvas, texture) in [(&mut first, 1, 1), (&mut second, 2, 2)] {
            budget.create_texture(canvas, texture).unwrap();
            budget.bind_texture(canvas, TEXTURE_2D, Some(texture));
            let allocation = budget
                .prepare_tex_image_2d(
                    canvas,
                    TEXTURE_2D,
                    0,
                    RGBA as i32,
                    4,
                    4,
                    0,
                    RGBA,
                    UNSIGNED_BYTE,
                )
                .unwrap();
            budget.commit(allocation);
        }
        assert_eq!(scope.process_usage(), 128);

        first.create_texture(1, 3).unwrap();
        first.bind_texture(1, TEXTURE_2D, Some(3));
        let second_in_context = first
            .prepare_tex_image_2d(1, TEXTURE_2D, 0, RGBA as i32, 4, 4, 0, RGBA, UNSIGNED_BYTE)
            .unwrap();
        first.commit(second_in_context);
        assert_eq!(scope.process_usage(), 192);

        first.create_texture(1, 4).unwrap();
        first.bind_texture(1, TEXTURE_2D, Some(4));
        assert_eq!(
            first
                .prepare_tex_image_2d(1, TEXTURE_2D, 0, RGBA as i32, 1, 1, 0, RGBA, UNSIGNED_BYTE,)
                .unwrap_err(),
            GpuAllocationError::OutOfMemory
        );

        second.create_texture(2, 5).unwrap();
        second.bind_texture(2, TEXTURE_2D, Some(5));
        assert_eq!(
            second
                .prepare_tex_image_2d(2, TEXTURE_2D, 0, RGBA as i32, 1, 1, 0, RGBA, UNSIGNED_BYTE,)
                .unwrap_err(),
            GpuAllocationError::OutOfMemory
        );
    }

    #[test]
    fn delete_and_context_teardown_release_exact_usage_and_bindings() {
        let scope = GpuBudgetTestScope::new(limits(256, 512));
        let mut budget = scope.registry();
        for (canvas, texture) in [(1, 11), (2, 22)] {
            budget.create_texture(canvas, texture).unwrap();
            budget.bind_texture(canvas, TEXTURE_2D, Some(texture));
            let allocation = budget
                .prepare_tex_image_2d(
                    canvas,
                    TEXTURE_2D,
                    0,
                    RGBA as i32,
                    4,
                    4,
                    0,
                    RGBA,
                    UNSIGNED_BYTE,
                )
                .unwrap();
            budget.commit(allocation);
        }
        assert_eq!(scope.process_usage(), 128);

        assert_eq!(budget.delete_texture(11), 64);
        assert_eq!(budget.delete_texture(11), 0, "delete is idempotent");
        assert_eq!(budget.context_usage(1), 0);
        assert_eq!(scope.process_usage(), 64);
        assert_eq!(
            budget
                .prepare_tex_image_2d(1, TEXTURE_2D, 0, RGBA as i32, 1, 1, 0, RGBA, UNSIGNED_BYTE,)
                .unwrap_err(),
            GpuAllocationError::InvalidOperation,
            "delete implicitly clears every matching binding"
        );

        assert_eq!(budget.release_context(2), 64);
        assert_eq!(budget.release_context(2), 0);
        assert_eq!(scope.process_usage(), 0);
        assert_eq!(budget.context_usage(2), 0);
    }

    #[test]
    fn invalid_active_texture_unit_cannot_redirect_object_accounting() {
        let scope = GpuBudgetTestScope::new(limits(256, 512));
        let mut budget = scope.registry();
        budget.create_texture(1, 11).unwrap();
        budget.create_texture(1, 12).unwrap();
        budget.bind_texture(1, TEXTURE_2D, Some(11));

        // GLES 3 exposes TEXTURE0..TEXTURE31. GL rejects TEXTURE32 and keeps
        // the prior unit active, so the accounting mirror must do the same.
        budget.active_texture(1, TEXTURE0 + 32);
        budget.bind_texture(1, TEXTURE_2D, Some(12));
        budget.active_texture(1, TEXTURE0);
        let allocation = budget
            .prepare_tex_image_2d(1, TEXTURE_2D, 0, RGBA as i32, 4, 4, 0, RGBA, UNSIGNED_BYTE)
            .unwrap();
        budget.commit(allocation);

        assert_eq!(budget.delete_texture(11), 0);
        assert_eq!(budget.context_usage(1), 64);
        assert_eq!(budget.delete_texture(12), 64);
        assert_eq!(scope.process_usage(), 0);
    }

    // ── The texture-unit ledger ──────────────────────────────────────────
    //
    // These cover the container that replaced `HashMap<(unit, target), _>`.
    // The two binding tests above pin one unit and one target between them,
    // which a ledger that ignored both keys entirely would still pass — so the
    // scoping is asserted here rather than assumed.

    /// Each `(unit, target)` pair is its own slot: an upload resolves to the
    /// texture bound at the *active* unit and the *requested* target, not to
    /// whatever was bound last.
    #[test]
    fn an_upload_resolves_the_texture_bound_at_its_own_unit_and_target() {
        let scope = GpuBudgetTestScope::new(limits(4_096, 8_192));
        let mut budget = scope.registry();
        for id in [21u32, 22, 23] {
            budget.create_texture(1, id).unwrap();
        }

        // Unit 0 / TEXTURE_2D, unit 3 / TEXTURE_2D, unit 0 / TEXTURE_CUBE_MAP.
        budget.active_texture(1, TEXTURE0);
        budget.bind_texture(1, TEXTURE_2D, Some(21));
        budget.active_texture(1, TEXTURE0 + 3);
        budget.bind_texture(1, TEXTURE_2D, Some(22));
        budget.active_texture(1, TEXTURE0);
        budget.bind_texture(1, TEXTURE_CUBE_MAP, Some(23));

        // Still on unit 0: a 2D upload must charge 21, and a cube upload 23.
        let a = budget
            .prepare_tex_image_2d(1, TEXTURE_2D, 0, RGBA as i32, 4, 4, 0, RGBA, UNSIGNED_BYTE)
            .unwrap();
        budget.commit(a);
        assert_eq!(
            budget.delete_texture(21),
            64,
            "the 2D upload on unit 0 was charged to another slot's texture"
        );

        budget.active_texture(1, TEXTURE0 + 3);
        let b = budget
            .prepare_tex_image_2d(1, TEXTURE_2D, 0, RGBA as i32, 2, 2, 0, RGBA, UNSIGNED_BYTE)
            .unwrap();
        budget.commit(b);
        assert_eq!(
            budget.delete_texture(22),
            16,
            "the 2D upload on unit 3 did not resolve unit 3's binding"
        );

        assert_eq!(budget.delete_texture(23), 0, "the cube texture was charged");
        assert_eq!(scope.process_usage(), 0);
    }

    /// The highest unit GLES 3 exposes has a slot; the existing tests only ever
    /// touch unit 0 and unit 3, so an off-by-one at the top of the range would
    /// go unnoticed.
    #[test]
    fn the_last_texture_unit_has_its_own_slot() {
        let scope = GpuBudgetTestScope::new(limits(1_024, 2_048));
        let mut budget = scope.registry();
        budget.create_texture(1, 31).unwrap();

        budget.active_texture(1, TEXTURE0 + 31);
        budget.bind_texture(1, TEXTURE_2D, Some(31));
        let allocation = budget
            .prepare_tex_image_2d(1, TEXTURE_2D, 0, RGBA as i32, 4, 4, 0, RGBA, UNSIGNED_BYTE)
            .unwrap();
        budget.commit(allocation);

        assert_eq!(budget.delete_texture(31), 64);
        assert_eq!(scope.process_usage(), 0);
    }

    /// Deleting a texture unbinds it everywhere (GLES 3.0 §3.8.14) and nowhere
    /// else. A sweep that cleared the whole ledger would make a later upload
    /// against a surviving binding fail with `InvalidOperation`.
    #[test]
    fn deleting_a_texture_clears_only_the_slots_that_named_it() {
        let scope = GpuBudgetTestScope::new(limits(4_096, 8_192));
        let mut budget = scope.registry();
        budget.create_texture(1, 41).unwrap();
        budget.create_texture(1, 42).unwrap();

        // 41 on two different units, 42 on a third.
        budget.active_texture(1, TEXTURE0);
        budget.bind_texture(1, TEXTURE_2D, Some(41));
        budget.active_texture(1, TEXTURE0 + 1);
        budget.bind_texture(1, TEXTURE_2D, Some(41));
        budget.active_texture(1, TEXTURE0 + 2);
        budget.bind_texture(1, TEXTURE_2D, Some(42));

        assert_eq!(budget.delete_texture(41), 0);

        // Both of 41's units are now unbound.
        for unit in [TEXTURE0, TEXTURE0 + 1] {
            budget.active_texture(1, unit);
            assert_eq!(
                budget
                    .prepare_tex_image_2d(
                        1,
                        TEXTURE_2D,
                        0,
                        RGBA as i32,
                        4,
                        4,
                        0,
                        RGBA,
                        UNSIGNED_BYTE
                    )
                    .unwrap_err(),
                GpuAllocationError::InvalidOperation,
                "unit {unit:#x} still names the deleted texture"
            );
        }

        // 42's unit is untouched and still usable.
        budget.active_texture(1, TEXTURE0 + 2);
        let allocation = budget
            .prepare_tex_image_2d(1, TEXTURE_2D, 0, RGBA as i32, 4, 4, 0, RGBA, UNSIGNED_BYTE)
            .expect("an unrelated binding was swept away by the delete");
        budget.commit(allocation);
        assert_eq!(budget.delete_texture(42), 64);
        assert_eq!(scope.process_usage(), 0);
    }

    /// Two contexts keep separate ledgers — the canvas-keyed table is what
    /// separates them, and a memo that answered without checking its key would
    /// charge one context's upload to the other's texture.
    #[test]
    fn two_contexts_keep_separate_texture_unit_ledgers() {
        let scope = GpuBudgetTestScope::new(limits(4_096, 8_192));
        let mut budget = scope.registry();
        budget.create_texture(1, 51).unwrap();
        budget.create_texture(2, 52).unwrap();

        budget.bind_texture(1, TEXTURE_2D, Some(51));
        budget.bind_texture(2, TEXTURE_2D, Some(52));

        // Alternate contexts so the memo is stale on both lookups.
        let a = budget
            .prepare_tex_image_2d(1, TEXTURE_2D, 0, RGBA as i32, 4, 4, 0, RGBA, UNSIGNED_BYTE)
            .unwrap();
        budget.commit(a);
        let b = budget
            .prepare_tex_image_2d(2, TEXTURE_2D, 0, RGBA as i32, 2, 2, 0, RGBA, UNSIGNED_BYTE)
            .unwrap();
        budget.commit(b);

        assert_eq!(budget.context_usage(1), 64, "context 1's charge is wrong");
        assert_eq!(budget.context_usage(2), 16, "context 2's charge is wrong");
        assert_eq!(budget.delete_texture(51), 64);
        assert_eq!(budget.delete_texture(52), 16);
        assert_eq!(scope.process_usage(), 0);
    }

    #[test]
    fn immutable_storage_cannot_be_redefined() {
        let scope = GpuBudgetTestScope::new(limits(1_024, 2_048));
        let mut budget = scope.registry();
        budget.create_texture(1, 1).unwrap();
        budget.bind_texture(1, TEXTURE_2D, Some(1));
        let immutable = budget
            .prepare_tex_storage_2d(1, TEXTURE_2D, 1, RGBA8, 4, 4)
            .unwrap();
        budget.commit(immutable);

        assert_eq!(
            budget
                .prepare_tex_storage_2d(1, TEXTURE_2D, 1, RGBA8, 4, 4)
                .unwrap_err(),
            GpuAllocationError::InvalidOperation
        );
        assert_eq!(
            budget
                .prepare_tex_image_2d(1, TEXTURE_2D, 0, RGBA as i32, 4, 4, 0, RGBA, UNSIGNED_BYTE,)
                .unwrap_err(),
            GpuAllocationError::InvalidOperation
        );
        assert_eq!(budget.context_usage(1), 64);
        assert_eq!(scope.process_usage(), 64);
    }

    #[test]
    fn cube_faces_are_independent_mutable_subresources() {
        let scope = GpuBudgetTestScope::new(limits(1_024, 2_048));
        let mut budget = scope.registry();
        budget.create_texture(1, 1).unwrap();
        budget.bind_texture(1, TEXTURE_CUBE_MAP, Some(1));

        for face in TEXTURE_CUBE_MAP_POSITIVE_X..=TEXTURE_CUBE_MAP_POSITIVE_X + 5 {
            let allocation = budget
                .prepare_tex_image_2d(1, face, 0, RGBA as i32, 2, 2, 0, RGBA, UNSIGNED_BYTE)
                .unwrap();
            budget.commit(allocation);
        }
        assert_eq!(budget.context_usage(1), 96);
        assert_eq!(scope.process_usage(), 96);
    }

    /// R6 – `CompressedTexImage2D` and `GenerateMipmap` must route through
    /// the ledger so their bytes count against the per-context and process limits.
    /// This test drove the addition of `prepare_compressed_tex_image_2d` and
    /// `prepare_generate_mipmap`; it was RED (compile error: no such method)
    /// before those methods were added.
    #[test]
    fn r6_compressed_and_mipmap_bytes_routed_through_ledger() {
        let scope = GpuBudgetTestScope::new(limits(1_024, 2_048));
        let mut budget = scope.registry();
        let mut gl = RecordingGl::default();

        // -- Compressed image: 4×4 block, 8 bytes compressed data ------------
        budget.create_texture(1, 1).unwrap();
        budget.bind_texture(1, TEXTURE_2D, Some(1));
        let c = budget
            .prepare_compressed_tex_image_2d(1, TEXTURE_2D, 0, 0x8C00_u32, 4, 4, 0, 8_u32)
            .expect("compressed alloc must succeed");
        assert_eq!(c.byte_len(), 8, "compressed bytes must equal image_size");
        budget.commit(c);
        gl.define(1, 0, 8);
        assert_eq!(budget.context_usage(1), gl.bytes());
        assert_eq!(scope.process_usage(), gl.bytes());

        // -- GenerateMipmap: base 4×4 RGBA8 = 64 bytes, full chain = 84 ------
        budget.create_texture(1, 2).unwrap();
        budget.bind_texture(1, TEXTURE_2D, Some(2));
        let base = budget
            .prepare_tex_image_2d(1, TEXTURE_2D, 0, RGBA as i32, 4, 4, 0, RGBA, UNSIGNED_BYTE)
            .unwrap();
        budget.commit(base);
        gl.define(2, 0, 64);
        assert_eq!(budget.context_usage(1), gl.bytes());
        assert_eq!(scope.process_usage(), gl.bytes());
        assert_eq!(budget.context_usage(1), 8 + 64);

        // Rollback: drop before commit must restore bytes exactly
        let before_rollback = scope.process_usage();
        let mip_rollback = budget
            .prepare_generate_mipmap(1, TEXTURE_2D)
            .expect("generate_mipmap must be accountable on a mutable texture");
        drop(mip_rollback);
        assert_eq!(
            scope.process_usage(),
            before_rollback,
            "rollback must restore process bytes"
        );
        assert_eq!(
            budget.context_usage(1),
            8 + 64,
            "context bytes unchanged after rollback"
        );

        // Commit: full mip chain for 4×4 RGBA8 is 64+16+4 = 84 bytes
        let mip = budget
            .prepare_generate_mipmap(1, TEXTURE_2D)
            .expect("generate_mipmap after rollback");
        budget.commit(mip);
        gl.define(2, 1, 16);
        gl.define(2, 2, 4);
        assert_eq!(budget.context_usage(1), gl.bytes());
        assert_eq!(scope.process_usage(), gl.bytes());
        let chain: u64 = 64 + 16 + 4; // levels: 4×4, 2×2, 1×1
        assert_eq!(budget.context_usage(1), 8 + chain);
        assert_eq!(scope.process_usage(), 8 + chain);

        // Redefine base level after generate_mipmap: texture is still mutable
        let redef = budget
            .prepare_tex_image_2d(1, TEXTURE_2D, 0, RGBA as i32, 2, 2, 0, RGBA, UNSIGNED_BYTE)
            .expect("redefine level-0 after generate_mipmap must be allowed");
        budget.commit(redef);
        gl.define(2, 0, 16);
        assert_eq!(budget.context_usage(1), gl.bytes());
        assert_eq!(scope.process_usage(), gl.bytes());
        // Old level-0 (64 B) is replaced by 2×2 (16 B); levels 1 and 2 stay.
        // New total for tex 2: 16 + 16 + 4 = 36
        assert_eq!(budget.context_usage(1), 8 + 36);
        assert_eq!(scope.process_usage(), 8 + 36);

        // Delete clears both textures cleanly
        budget.delete_texture(1);
        budget.delete_texture(2);
        assert_eq!(scope.process_usage(), 0);
    }
}
