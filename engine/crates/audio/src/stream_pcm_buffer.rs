//! A recyclable PCM chunk and the in-flight slot that bounds how many exist.
use std::ops::{Deref, DerefMut};

#[derive(Default)]
pub(super) struct Buffer {
    buffer: Vec<f32>,
    _slot: Option<tokio::sync::OwnedSemaphorePermit>,
}
impl Buffer {
    pub(super) fn new(buffer: Vec<f32>, slot: Option<tokio::sync::OwnedSemaphorePermit>) -> Self {
        Self {
            buffer,
            _slot: slot,
        }
    }
}
impl Deref for Buffer {
    type Target = Vec<f32>;
    fn deref(&self) -> &Vec<f32> {
        &self.buffer
    }
}
impl DerefMut for Buffer {
    fn deref_mut(&mut self) -> &mut Vec<f32> {
        &mut self.buffer
    }
}
