//! Actor context buckets reserve before the factory; route invalidation stays synchronous.
use super::*;
use crate::graph_map::{GraphMap, Initial, Plan};
use crate::node_retirement::Host;
pub(super) struct ContextTable {
    contexts: GraphMap<AudioContext>,
    host: Host,
}
impl ContextTable {
    pub(super) fn new(host: Host) -> EngineResult<Self> {
        let initial = Initial::reserve(&host, 4)?;
        Ok(Self {
            contexts: initial.finish(),
            host,
        })
    }
    pub(super) fn create_with(
        &mut self,
        id: AudioContextId,
        factory: impl FnOnce() -> EngineResult<AudioContext>,
    ) -> EngineResult<bool> {
        if self.contexts.contains_key(&id) {
            return Ok(false);
        }
        let plan = Plan::reserve(&self.contexts, Some(&self.host), self.contexts.len() + 1)?;
        let context = factory()?;
        if let Some(plan) = plan {
            plan.install(&mut self.contexts);
        }
        self.contexts.insert(id, context);
        Ok(true)
    }
    #[cfg(test)]
    pub(super) fn identity(&self) -> (usize, usize, Vec<(u32, usize)>) {
        let mut rows: Vec<_> = self
            .contexts
            .iter()
            .map(|(id, ctx)| (*id, ctx as *const AudioContext as usize))
            .collect();
        rows.sort_unstable();
        (self.contexts.len(), self.contexts.capacity(), rows)
    }
    #[cfg(test)]
    pub(super) fn table_bytes(&self) -> usize {
        self.contexts.allocation_bytes()
    }
    #[cfg(test)]
    pub(super) fn observe_table(&mut self, hook: Arc<dyn Fn() + Send + Sync>) {
        self.contexts.observe(hook);
    }
}
impl Drop for ContextTable {
    fn drop(&mut self) {
        // AudioContext Drop immediately revokes the device epoch and hands its
        // existing storage to its original CPU. Only empty buckets wait here.
        for (_, context) in self.contexts.drain() {
            drop(context);
        }
    }
}
pub(crate) trait ContextView {
    fn view(&self) -> &HashMap<AudioContextId, AudioContext>;
    fn get(&self, id: &AudioContextId) -> Option<&AudioContext> {
        self.view().get(id)
    }
    fn contains_key(&self, id: &AudioContextId) -> bool {
        self.view().contains_key(id)
    }
    fn values(&self) -> std::collections::hash_map::Values<'_, AudioContextId, AudioContext> {
        self.view().values()
    }
    fn iter(&self) -> std::collections::hash_map::Iter<'_, AudioContextId, AudioContext> {
        self.view().iter()
    }
}
pub(crate) trait ContextAccess: ContextView {
    fn get_mut(&mut self, id: &AudioContextId) -> Option<&mut AudioContext>;
    fn values_mut(
        &mut self,
    ) -> std::collections::hash_map::ValuesMut<'_, AudioContextId, AudioContext>;
    fn remove(&mut self, id: &AudioContextId) -> Option<AudioContext>;
    fn drain(&mut self) -> std::collections::hash_map::Drain<'_, AudioContextId, AudioContext>;
    fn iter_mut(&mut self)
    -> std::collections::hash_map::IterMut<'_, AudioContextId, AudioContext>;
}
pub(crate) trait ContextCreate: ContextAccess {
    fn create_context(
        &mut self,
        id: AudioContextId,
        rate: u32,
        channels: u32,
        cpu: Option<&migo_io::audio_cpu::AudioCpu>,
        aggregate: impl FnOnce() -> Arc<AudioAggregateLedger>,
    ) -> bool;
}
impl ContextView for ContextTable {
    fn view(&self) -> &HashMap<AudioContextId, AudioContext> {
        &self.contexts
    }
}
impl ContextAccess for ContextTable {
    fn get_mut(&mut self, id: &AudioContextId) -> Option<&mut AudioContext> {
        self.contexts.get_mut(id)
    }
    fn values_mut(
        &mut self,
    ) -> std::collections::hash_map::ValuesMut<'_, AudioContextId, AudioContext> {
        self.contexts.values_mut()
    }
    fn remove(&mut self, id: &AudioContextId) -> Option<AudioContext> {
        self.contexts.remove(id)
    }
    fn drain(&mut self) -> std::collections::hash_map::Drain<'_, AudioContextId, AudioContext> {
        self.contexts.drain()
    }
    fn iter_mut(
        &mut self,
    ) -> std::collections::hash_map::IterMut<'_, AudioContextId, AudioContext> {
        self.contexts.iter_mut()
    }
}
impl ContextCreate for ContextTable {
    fn create_context(
        &mut self,
        id: AudioContextId,
        rate: u32,
        channels: u32,
        cpu: Option<&migo_io::audio_cpu::AudioCpu>,
        aggregate: impl FnOnce() -> Arc<AudioAggregateLedger>,
    ) -> bool {
        match self.create_with(id, || match cpu {
            Some(cpu) => AudioContext::new_managed(id, rate, channels, cpu.clone(), aggregate()),
            None => Ok(AudioContext::new(id, rate, channels)),
        }) {
            Ok(created) => created,
            Err(error) => {
                tracing::warn!(
                    "AudioContext {id} table or construction admission rejected: {error}"
                );
                false
            }
        }
    }
}
#[cfg(test)]
impl ContextView for HashMap<AudioContextId, AudioContext> {
    fn view(&self) -> &HashMap<AudioContextId, AudioContext> {
        self
    }
}
#[cfg(test)]
impl ContextAccess for HashMap<AudioContextId, AudioContext> {
    fn get_mut(&mut self, id: &AudioContextId) -> Option<&mut AudioContext> {
        self.get_mut(id)
    }
    fn values_mut(
        &mut self,
    ) -> std::collections::hash_map::ValuesMut<'_, AudioContextId, AudioContext> {
        self.values_mut()
    }
    fn remove(&mut self, id: &AudioContextId) -> Option<AudioContext> {
        self.remove(id)
    }
    fn drain(&mut self) -> std::collections::hash_map::Drain<'_, AudioContextId, AudioContext> {
        self.drain()
    }
    fn iter_mut(
        &mut self,
    ) -> std::collections::hash_map::IterMut<'_, AudioContextId, AudioContext> {
        self.iter_mut()
    }
}
#[cfg(test)]
impl ContextCreate for HashMap<AudioContextId, AudioContext> {
    fn create_context(
        &mut self,
        id: AudioContextId,
        rate: u32,
        channels: u32,
        cpu: Option<&migo_io::audio_cpu::AudioCpu>,
        aggregate: impl FnOnce() -> Arc<AudioAggregateLedger>,
    ) -> bool {
        super::create_context_scoped_with_ledger(self, id, rate, channels, cpu, aggregate)
    }
}
