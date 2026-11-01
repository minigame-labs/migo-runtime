//! Fixed-shape node data owns admission until its actual CPU destructor.
//! Retained vectors must not grow after construction or adoption.
use crate::node_retirement::{Host, Reservation, Value};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::ops::{Deref, DerefMut};

pub(crate) trait CapacityBytes {
    fn capacity_bytes(&self) -> usize;
}
impl<T> CapacityBytes for Vec<T> {
    fn capacity_bytes(&self) -> usize {
        self.capacity() * std::mem::size_of::<T>()
    }
}
pub(crate) fn size_overflow() -> EngineError {
    EngineError::from_detail(ErrorCode::InputSaturated, "node storage size overflow")
}
pub(crate) struct FixedStorage<T: Send + 'static>(Value<T>);
impl<T: CapacityBytes + Send + 'static> FixedStorage<T> {
    pub(crate) fn new(
        host: Option<&Host>,
        bytes: usize,
        create: impl FnOnce() -> T,
    ) -> EngineResult<Self> {
        let reservation = host.map_or_else(
            || Ok(Reservation::unmanaged()),
            |host| host.reserve_value_with_bytes::<T>(bytes),
        )?;
        let data = create();
        debug_assert_eq!(data.capacity_bytes(), bytes);
        Ok(Self(reservation.finish(data)))
    }
    pub(crate) fn prepare(&mut self, host: &Host) -> EngineResult<()> {
        if !self.0.accepts_physical_ledger(host) {
            return Err(EngineError::from_detail(
                ErrorCode::InvalidArgument,
                "node storage belongs to another physical ledger",
            ));
        }
        self.0.prepare_with_bytes(host, self.0.capacity_bytes())
    }
    #[cfg(test)]
    pub(crate) fn observe(&mut self, hook: std::sync::Arc<dyn Fn() + Send + Sync>) {
        self.0.observe(hook);
    }
}
impl<T: Send + 'static> Deref for FixedStorage<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}
impl<T: Send + 'static> DerefMut for FixedStorage<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}
