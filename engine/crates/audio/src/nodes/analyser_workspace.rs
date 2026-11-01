//! All FFT capacities share one pre-admitted owner and CPU retirement lease.
use crate::node_retirement::{Host, Reservation, Value};
use shared::error::EngineResult;

pub(super) struct Workspace {
    pub(super) time_domain_buffer: Vec<f32>,
    pub(super) fft_re: Vec<f64>,
    pub(super) fft_im: Vec<f64>,
    pub(super) window: Vec<f64>,
    pub(super) twiddles: Vec<(f64, f64)>,
    pub(super) smoothed: Vec<f64>,
}
impl Workspace {
    pub(super) fn reserve(host: Option<&Host>, size: usize) -> EngineResult<Reservation<Self>> {
        // fftSize is normalized to 32..=32768 before this multiplication.
        let bytes = size * 40;
        host.map_or_else(
            || Ok(Reservation::unmanaged()),
            |host| host.reserve_value_with_bytes::<Self>(bytes),
        )
    }
    pub(super) fn new(size: usize) -> Self {
        Self {
            time_domain_buffer: vec![0.0; size],
            fft_re: vec![0.0; size],
            fft_im: vec![0.0; size],
            window: super::AnalyserNode::compute_blackman_window(size),
            twiddles: super::AnalyserNode::compute_twiddles(size),
            smoothed: vec![0.0; size / 2],
        }
    }
    pub(super) fn capacity_bytes(&self) -> usize {
        self.time_domain_buffer.capacity() * 4
            + self.fft_re.capacity() * 8
            + self.fft_im.capacity() * 8
            + self.window.capacity() * 8
            + self.twiddles.capacity() * 16
            + self.smoothed.capacity() * 8
    }
    pub(super) fn replacement(
        previous: &Self,
        size: usize,
        host: Option<&Host>,
    ) -> EngineResult<Value<Self>> {
        let reservation = Self::reserve(host, size)?;
        let mut next = Self::new(size);
        // The old resize preserved each leading prefix and reset write_pos.
        // Keep that history even though the new allocations have exact capacity.
        let keep = size.min(previous.time_domain_buffer.len());
        next.time_domain_buffer[..keep].copy_from_slice(&previous.time_domain_buffer[..keep]);
        next.fft_re[..keep].copy_from_slice(&previous.fft_re[..keep]);
        next.fft_im[..keep].copy_from_slice(&previous.fft_im[..keep]);
        debug_assert_eq!(next.capacity_bytes(), size * 40);
        Ok(reservation.finish(next))
    }
}
