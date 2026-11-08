//! Reserve the actor table before creating a player; cancel players on the caller.
use super::*;

#[derive(Default)]
pub(super) struct InnerPlayers {
    players: HashMap<InnerAudioId, InnerAudioPlayer>,
}
impl InnerPlayers {
    pub(super) fn new() -> Self {
        Self {
            players: HashMap::with_capacity(16),
        }
    }
    pub(super) fn create_with(
        &mut self,
        id: InnerAudioId,
        factory: impl FnOnce() -> EngineResult<InnerAudioPlayer>,
    ) -> EngineResult<bool> {
        if self.players.contains_key(&id) {
            return Ok(false);
        }
        self.players.insert(id, factory()?);
        Ok(true)
    }
    pub(super) fn remove(&mut self, id: &InnerAudioId) -> Option<InnerAudioPlayer> {
        self.players.remove(id)
    }
}

pub(crate) trait PlayerView {
    fn view(&self) -> &HashMap<InnerAudioId, InnerAudioPlayer>;
    fn get(&self, id: &InnerAudioId) -> Option<&InnerAudioPlayer> {
        self.view().get(id)
    }
    fn values(&self) -> std::collections::hash_map::Values<'_, InnerAudioId, InnerAudioPlayer> {
        self.view().values()
    }
}
pub(crate) trait PlayerAccess: PlayerView {
    fn get_mut(&mut self, id: &InnerAudioId) -> Option<&mut InnerAudioPlayer>;
    fn values_mut(
        &mut self,
    ) -> std::collections::hash_map::ValuesMut<'_, InnerAudioId, InnerAudioPlayer>;
}
impl PlayerView for InnerPlayers {
    fn view(&self) -> &HashMap<InnerAudioId, InnerAudioPlayer> {
        &self.players
    }
}
impl PlayerAccess for InnerPlayers {
    fn get_mut(&mut self, id: &InnerAudioId) -> Option<&mut InnerAudioPlayer> {
        self.players.get_mut(id)
    }
    fn values_mut(
        &mut self,
    ) -> std::collections::hash_map::ValuesMut<'_, InnerAudioId, InnerAudioPlayer> {
        self.players.values_mut()
    }
}
#[cfg(test)]
impl PlayerView for HashMap<InnerAudioId, InnerAudioPlayer> {
    fn view(&self) -> &HashMap<InnerAudioId, InnerAudioPlayer> {
        self
    }
}
#[cfg(test)]
impl PlayerAccess for HashMap<InnerAudioId, InnerAudioPlayer> {
    fn get_mut(&mut self, id: &InnerAudioId) -> Option<&mut InnerAudioPlayer> {
        self.get_mut(id)
    }
    fn values_mut(
        &mut self,
    ) -> std::collections::hash_map::ValuesMut<'_, InnerAudioId, InnerAudioPlayer> {
        self.values_mut()
    }
}
