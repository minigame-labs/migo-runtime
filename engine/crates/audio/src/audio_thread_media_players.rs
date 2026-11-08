//! Media membership keeps its standard table and source-vector allocation credit.
use super::*;

#[derive(Default)]
pub(super) struct MediaPlayers {
    players: HashMap<u32, Vec<InnerAudioId>>,
}
impl MediaPlayers {
    pub(super) fn new() -> Self {
        Self {
            players: HashMap::with_capacity(4),
        }
    }
    pub(super) fn create(&mut self, id: u32) {
        self.players.insert(id, Vec::new());
    }
    pub(super) fn add_source(&mut self, id: u32, source: InnerAudioId) {
        if let Some(sources) = self.players.get_mut(&id)
            && !sources.contains(&source)
        {
            sources.push(source);
        }
    }
    pub(super) fn remove_source(&mut self, id: u32, source: InnerAudioId) {
        if let Some(sources) = self.players.get_mut(&id) {
            sources.retain(|value| *value != source);
        }
    }
    pub(super) fn get(&self, id: &u32) -> Option<&[InnerAudioId]> {
        self.players.get(id).map(Vec::as_slice)
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
