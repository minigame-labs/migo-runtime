//! Standard hash tables keep allocation credit even when tombstones reduce capacity.
use crate::graph_table_layout::Layout;
use crate::limits::AudioAggregatePermit;
use crate::node_retirement::{Host, Reservation, Value};
use shared::error::{EngineError, EngineResult, ErrorCode};
use std::collections::HashMap;
use std::ops::Deref;

type Data<V> = HashMap<u32, V>;
enum Owner<V: Send + 'static> {
    Native,
    Inline(AudioAggregatePermit),
    Cpu(Value<Option<Data<V>>>),
}
enum Admission<V: Send + 'static> {
    Inline(AudioAggregatePermit),
    Cpu(Reservation<Option<Data<V>>>),
}
impl<V: Send + 'static> Admission<V> {
    fn reserve(host: &Host, bytes: usize) -> EngineResult<Self> {
        if bytes < 4096 {
            Ok(Self::Inline(host.reserve_inline(bytes)?))
        } else {
            Ok(Self::Cpu(
                host.reserve_value_with_bytes::<Option<Data<V>>>(bytes)?,
            ))
        }
    }
    fn finish(self) -> Owner<V> {
        match self {
            Self::Inline(permit) => Owner::Inline(permit),
            Self::Cpu(reservation) => Owner::Cpu(reservation.finish(None)),
        }
    }
}
/// Tickets for a fresh standard table; no buckets exist until finish.
pub(crate) struct Initial<V: Send + 'static> {
    layout: Layout,
    admission: Admission<V>,
}
impl<V: Send + 'static> Initial<V> {
    pub(crate) fn reserve(host: &Host, entries: usize) -> EngineResult<Self> {
        let layout = Layout::for_entries::<(u32, V)>(entries)?;
        Ok(Self {
            layout,
            admission: Admission::reserve(host, layout.bytes)?,
        })
    }
    pub(crate) fn finish(self) -> GraphMap<V> {
        let data = HashMap::with_capacity(self.layout.capacity);
        assert_eq!(
            data.capacity(),
            self.layout.capacity,
            "standard hash allocation model changed"
        );
        GraphMap {
            data,
            layout: self.layout,
            owner: self.admission.finish(),
        }
    }
}
pub(crate) struct GraphMap<V: Send + 'static> {
    // data drops before inline credit; CPU owners receive the same table in Drop.
    data: Data<V>,
    layout: Layout,
    owner: Owner<V>,
}
pub(crate) struct Plan<V: Send + 'static> {
    layout: Layout,
    admission: Option<Admission<V>>,
    required: usize,
}
impl<V: Send + 'static> Plan<V> {
    pub(crate) fn reserve(
        data: &GraphMap<V>,
        host: Option<&Host>,
        required: usize,
    ) -> EngineResult<Option<Self>> {
        if required <= data.capacity() {
            return Ok(None);
        }
        let layout = Layout::for_entries::<(u32, V)>(required.max(data.layout.capacity))?;
        let admission = host
            .map(|host| Admission::reserve(host, layout.bytes))
            .transpose()?;
        Ok(Some(Self {
            layout,
            admission,
            required,
        }))
    }
    pub(crate) fn install(self, target: &mut GraphMap<V>) {
        let Some(admission) = self.admission else {
            target.reserve_native(self.required);
            return;
        };
        let mut data =
            HashMap::with_capacity_and_hasher(self.layout.capacity, target.data.hasher().clone());
        assert_eq!(
            data.capacity(),
            self.layout.capacity,
            "standard hash allocation model changed"
        );
        for (key, value) in target.data.drain() {
            data.insert(key, value);
        }
        *target = GraphMap {
            data,
            layout: self.layout,
            owner: admission.finish(),
        };
    }
}
impl<V: Send + 'static> GraphMap<V> {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        let data = HashMap::with_capacity(capacity);
        let layout = Layout::for_entries::<(u32, V)>(data.capacity())
            .expect("native hash table layout overflow");
        Self {
            data,
            layout,
            owner: Owner::Native,
        }
    }
    pub(crate) fn new() -> Self {
        Self::with_capacity(0)
    }
    fn track_native_growth(&mut self) {
        // No shrink API escapes this wrapper. Only a fresh/larger allocation can
        // exceed the last full capacity; deletes/retain/drain keep the old bytes.
        if self.data.capacity() > self.layout.capacity {
            self.layout = Layout::for_entries::<(u32, V)>(self.data.capacity())
                .expect("native hash table layout overflow");
        }
    }
    pub(crate) fn reserve_native(&mut self, required: usize) {
        assert!(matches!(self.owner, Owner::Native));
        self.data.reserve(required.saturating_sub(self.data.len()));
        self.track_native_growth();
    }
    pub(crate) fn prepare(&mut self, host: &Host) -> EngineResult<()> {
        let accepts = match &self.owner {
            Owner::Native => true,
            Owner::Inline(permit) => host.accepts_inline(permit),
            Owner::Cpu(value) => value.accepts_physical_ledger(host),
        };
        if !accepts {
            return Err(EngineError::from_detail(
                ErrorCode::InvalidArgument,
                "graph table belongs to another physical ledger",
            ));
        }
        if matches!(self.owner, Owner::Native) && self.layout.bytes != 0 {
            self.owner = Admission::reserve(host, self.layout.bytes)?.finish();
        }
        Ok(())
    }
    pub(crate) fn insert(&mut self, key: u32, value: V) -> Option<V> {
        // std insert can grow a full table before discovering an existing key.
        if let Some(old) = self.data.get_mut(&key) {
            return Some(std::mem::replace(old, value));
        }
        assert!(
            matches!(self.owner, Owner::Native) || self.data.len() < self.data.capacity(),
            "managed graph table exceeded admitted capacity"
        );
        let old = self.data.insert(key, value);
        if matches!(self.owner, Owner::Native) {
            self.track_native_growth();
        }
        old
    }
    pub(crate) fn get_mut(&mut self, key: &u32) -> Option<&mut V> {
        self.data.get_mut(key)
    }
    pub(crate) fn values_mut(&mut self) -> std::collections::hash_map::ValuesMut<'_, u32, V> {
        self.data.values_mut()
    }
    pub(crate) fn iter_mut(&mut self) -> std::collections::hash_map::IterMut<'_, u32, V> {
        self.data.iter_mut()
    }
    pub(crate) fn remove(&mut self, key: &u32) -> Option<V> {
        self.data.remove(key)
    }
    pub(crate) fn drain(&mut self) -> std::collections::hash_map::Drain<'_, u32, V> {
        self.data.drain()
    }
    pub(crate) fn clear(&mut self) {
        self.data.clear();
    }
    pub(crate) fn retain(&mut self, keep: impl FnMut(&u32, &mut V) -> bool) {
        self.data.retain(keep);
    }
    #[cfg(test)]
    pub(crate) fn set_native_capacity(&mut self, capacity: usize) {
        assert!(matches!(self.owner, Owner::Native));
        let mut data =
            HashMap::with_capacity_and_hasher(capacity.max(self.len()), self.data.hasher().clone());
        data.extend(self.data.drain());
        self.data = data;
        self.layout = Layout::for_entries::<(u32, V)>(self.data.capacity()).unwrap();
    }
    #[cfg(test)]
    pub(crate) fn allocation_bytes(&self) -> usize {
        self.layout.bytes
    }
    #[cfg(test)]
    pub(crate) fn observe(&mut self, hook: std::sync::Arc<dyn Fn() + Send + Sync>) {
        if let Owner::Cpu(value) = &mut self.owner {
            value.observe(hook);
        }
    }
}
impl<V: Send + 'static> Default for GraphMap<V> {
    fn default() -> Self {
        Self::new()
    }
}
impl<V: Send + 'static> Drop for GraphMap<V> {
    fn drop(&mut self) {
        if let Owner::Cpu(value) = &mut self.owner {
            **value = Some(std::mem::take(&mut self.data));
        }
    }
}
impl<V: Send + 'static> Deref for GraphMap<V> {
    type Target = Data<V>;
    fn deref(&self) -> &Self::Target {
        &self.data
    }
}
impl<V: Send + std::fmt::Debug + 'static> std::fmt::Debug for GraphMap<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.data.fmt(f)
    }
}
#[derive(Default)]
pub(crate) struct GraphSet {
    pub(super) map: GraphMap<()>,
}
impl GraphSet {
    pub(crate) fn new() -> Self {
        Self::default()
    }
    pub(crate) fn insert(&mut self, key: u32) -> bool {
        self.map.insert(key, ()).is_none()
    }
    pub(crate) fn remove(&mut self, key: &u32) -> bool {
        self.map.remove(key).is_some()
    }
    pub(crate) fn contains(&self, key: &u32) -> bool {
        self.map.contains_key(key)
    }
    #[cfg(test)]
    pub(crate) fn iter(&self) -> std::collections::hash_map::Keys<'_, u32, ()> {
        self.map.keys()
    }
    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }
    pub(crate) fn clear(&mut self) {
        self.map.clear();
    }
}
