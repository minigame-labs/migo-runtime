//! Media membership keeps its standard table and source-vector allocation credit.
use super::*;
use crate::graph_map::{GraphMap, Initial, Plan};
use crate::graph_vec::{GraphVec, Plan as VectorPlan};
use crate::node_retirement::Host;

#[cfg(test)]
type Identity = (usize, usize, Vec<(u32, Vec<u32>, usize, usize)>);

pub(super) struct MediaPlayers {
    players: GraphMap<GraphVec<InnerAudioId>>,
    host: Host,
}
impl MediaPlayers {
    pub(super) fn new(host: Host) -> EngineResult<Self> {
        let initial = Initial::reserve(&host, 4)?;
        Ok(Self {
            players: initial.finish(),
            host,
        })
    }
    pub(super) fn create(&mut self, id: u32) -> EngineResult<()> {
        let required = self.players.len() + usize::from(!self.players.contains_key(&id));
        let plan = Plan::reserve(&self.players, Some(&self.host), required)?;
        if let Some(plan) = plan {
            plan.install(&mut self.players);
        }
        // Repeated create historically resets membership, even for a live id.
        self.players.insert(id, GraphVec::new());
        Ok(())
    }
    pub(super) fn add_source(&mut self, id: u32, source: InnerAudioId) -> EngineResult<()> {
        let Some(sources) = self.players.get_mut(&id) else {
            return Ok(());
        };
        if sources.contains(&source) {
            return Ok(());
        }
        let plan = VectorPlan::reserve(sources, Some(&self.host), sources.len() + 1)?;
        if let Some(plan) = plan {
            plan.install(sources);
        }
        sources.push(source);
        Ok(())
    }
    pub(super) fn remove_source(&mut self, id: u32, source: InnerAudioId) {
        if let Some(sources) = self.players.get_mut(&id) {
            sources.retain(|value| *value != source);
        }
    }
    pub(super) fn get(&self, id: &u32) -> Option<&[InnerAudioId]> {
        self.players.get(id).map(GraphVec::as_slice)
    }
    pub(super) fn start(&self, id: u32, inner: &mut impl PlayerAccess) -> bool {
        let Some(sources) = self.get(&id) else {
            return false;
        };
        for &source in sources {
            if let Some(player) = inner.get_mut(&source) {
                player.play();
            }
        }
        true
    }
    pub(super) fn stop(&self, id: u32, inner: &mut impl PlayerAccess) {
        if let Some(sources) = self.get(&id) {
            for &source in sources {
                if let Some(player) = inner.get_mut(&source) {
                    player.stop();
                }
            }
        }
    }
    pub(super) fn destroy(&mut self, id: u32) {
        self.players.remove(&id);
    }
    #[cfg(test)]
    pub(super) fn identity(&self) -> Identity {
        let mut rows: Vec<_> = self
            .players
            .iter()
            .map(|(id, sources)| {
                (
                    *id,
                    sources.to_vec(),
                    sources.as_ptr() as usize,
                    sources.capacity(),
                )
            })
            .collect();
        rows.sort_unstable_by_key(|row| row.0);
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
    #[cfg(test)]
    pub(super) fn observe_sources(&mut self, id: u32, hook: Arc<dyn Fn() + Send + Sync>) {
        self.players.get_mut(&id).unwrap().observe(hook);
    }
}

#[cfg(any(target_os = "ios", test))]
pub(super) trait Lookup {
    fn get(&self, id: &u32) -> Option<&[InnerAudioId]>;
}
#[cfg(any(target_os = "ios", test))]
impl Lookup for MediaPlayers {
    fn get(&self, id: &u32) -> Option<&[InnerAudioId]> {
        self.get(id)
    }
}
#[cfg(test)]
impl Lookup for HashMap<u32, Vec<InnerAudioId>> {
    fn get(&self, id: &u32) -> Option<&[InnerAudioId]> {
        self.get(id).map(Vec::as_slice)
    }
}
