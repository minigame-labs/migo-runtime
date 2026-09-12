// Section 7.3's steady-state allocation gate reads this. `#[cfg(test)]` scopes it
// to this crate's own test binary: a `#[global_allocator]` is unique per binary, so
// one declared unconditionally here would follow the library into every shipped
// cdylib. Deleting it does not make the gates pass silently -- each burst proves the
// allocator is installed before it trusts a zero count.
#[cfg(test)]
#[global_allocator]
static COUNTING_ALLOCATOR: migo_alloc_probe::CountingAllocator =
    migo_alloc_probe::CountingAllocator::system();

pub mod astc;
pub mod atomic_write;
pub mod cost;
pub mod count_min_sketch;
pub mod derived_cache;
pub mod domain;
pub mod etc2;
mod fast_image_decoder;
pub mod fs_ops;
pub mod image_cache;
pub mod image_ops;
#[cfg(feature = "zip-extract")]
#[cfg(feature = "rust-image-decode")]
mod ingest_transcode;
pub mod ktx2;
pub mod kv_store;
pub mod mipmap;
pub mod mmap_reader;
#[cfg(feature = "zip-extract")]
pub mod package_ingest;
pub mod pools;
pub mod scheduler;
pub mod storage_ops;
pub mod task;
#[cfg(feature = "zip-extract")]
mod zip_extract;

pub use derived_cache::{
    DEFAULT_DERIVED_CACHE_MAX_BYTES, PruneReport, prune_derived_cache, schedule_derived_cache_prune,
};
pub use fast_image_decoder::{
    CompressedImageInfo, crop_image, decode_image_fast, decode_image_to_any,
    detect_compressed_format, probe_image_dimensions, register_platform_ahb_decoder,
    register_platform_decoder, resize_capable, try_resize_image,
};
pub use image_cache::{CacheStats, ImageCache, global_cache};
#[cfg(feature = "zip-extract")]
pub use package_ingest::{
    ingest_zip_to_package, ingest_zip_to_package_with_budget, ingest_zip_to_package_with_scheduler,
    ingest_zip_to_package_with_scheduler_and_budget,
};
#[cfg(feature = "zip-extract")]
pub use zip_extract::{
    ExtractBudget, ZipError, extract_zip, extract_zip_async, extract_zip_with_budget,
    extract_zip_with_scheduler, extract_zip_with_scheduler_and_budget,
};
