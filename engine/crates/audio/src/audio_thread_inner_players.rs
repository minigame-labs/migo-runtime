//! Reserve the actor table before creating a player; cancel players on the caller.
use super::*;
use crate::graph_map::{GraphMap, Initial, Plan};
use crate::node_retirement::Host;

pub(super) struct InnerPlayers {
    players: GraphMap<InnerAudioPlayer>,
    host: Host,
}
impl InnerPlayers {
    pub(super) fn new(host: Host) -> EngineResult<Self> {
        let initial = Initial::reserve(&host, 16)?;
        Ok(Self {
            players: initial.finish(),
            host,
        })
    }
    pub(super) fn create_with(
        &mut self,
        id: InnerAudioId,
        factory: impl FnOnce() -> EngineResult<InnerAudioPlayer>,
    ) -> EngineResult<bool> {
        if self.players.contains_key(&id) {
            return Ok(false);
        }
        let plan = Plan::reserve(&self.players, Some(&self.host), self.players.len() + 1)?;
        let player = factory()?;
        if let Some(plan) = plan {
            plan.install(&mut self.players);
        }
        self.players.insert(id, player);
        Ok(true)
    }
    pub(super) fn remove(&mut self, id: &InnerAudioId) -> Option<InnerAudioPlayer> {
        self.players.remove(id)
    }
    #[cfg(test)]
    pub(super) fn identity(&self) -> (usize, usize, Vec<(u32, usize, usize)>) {
        let mut rows: Vec<_> = self
            .players
            .iter()
            .map(|(id, player)| {
                (
                    *id,
                    player as *const InnerAudioPlayer as usize,
                    Arc::as_ptr(&player.shared) as usize,
                )
            })
            .collect();
        rows.sort_unstable();
        (self.players.len(), self.players.capacity(), rows)
    }
    #[cfg(test)]
    pub(super) fn table_bytes(&self) -> usize {
        self.players.allocation_bytes()
    }
    #[cfg(test)]
    pub(super) fn observe_table(&mut self, hook: Arc<dyn Fn() + Send + Sync>) {
        self.players.observe(hook);
    }
}
impl Drop for InnerPlayers {
    fn drop(&mut self) {
        // Player Drop invalidates source loads and cancels streams immediately.
        // The now-empty bulk table retains its credit through CPU destruction.
        for (_, player) in self.players.drain() {
            drop(player);
        }
    }
}

pub(crate) trait PlayerView {
    fn view(&self) -> &HashMap<InnerAudioId, InnerAudioPlayer>;
    fn get(&self, id: &InnerAudioId) -> Option<&InnerAudioPlayer> {
        self.view().get(id)
    }
    fn contains_key(&self, id: &InnerAudioId) -> bool {
        self.view().contains_key(id)
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
