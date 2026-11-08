//! The actor's contexts, keyed by their externally allocated ids.
use super::*;
pub(crate) trait ContextView {
    fn view(&self) -> &HashMap<AudioContextId, AudioContext>;
    fn get(&self, id: &AudioContextId) -> Option<&AudioContext> {
        self.view().get(id)
    }
    fn contains_key(&self, id: &AudioContextId) -> bool {
        self.view().contains_key(id)
    }
    #[cfg(test)]
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
    fn create_context(&mut self, id: AudioContextId, rate: u32, channels: u32) -> bool;
}
impl ContextView for HashMap<AudioContextId, AudioContext> {
    fn view(&self) -> &HashMap<AudioContextId, AudioContext> {
        self
    }
}
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
impl ContextCreate for HashMap<AudioContextId, AudioContext> {
    fn create_context(&mut self, id: AudioContextId, rate: u32, channels: u32) -> bool {
        if self.contains_key(&id) {
            return false;
        }
        self.insert(id, AudioContext::new(id, rate, channels));
        true
    }
}
