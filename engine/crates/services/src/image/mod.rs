//! Images: the alias table, inline sources, and loading.
//!
//! Moved from the embedded runtime's image ops so both executions load images
//! by the same rules; see [`loader`].

pub mod cache;
pub mod gl;
pub mod inline;
pub mod loader;

pub use loader::{
    ImageEnv, PreloadEntry, clear_image_cache, destroy_image, load_image, load_image_subrect,
    preload_images,
};
