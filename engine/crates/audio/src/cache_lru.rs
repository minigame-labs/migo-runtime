//! LRU links live in the already admitted cache buckets, without tree nodes.
use super::{Key, entries_storage::Entries};

#[derive(Default)]
pub(super) struct Links {
    older: Option<Key>,
    newer: Option<Key>,
}

#[derive(Default)]
pub(super) struct Lru {
    first: Option<Key>,
    last: Option<Key>,
}
impl Lru {
    pub(super) fn append(&mut self, entries: &mut Entries, key: Key) {
        let older = self.last.take();
        if let Some(previous) = &older {
            entries
                .get_mut(previous.as_str())
                .expect("cache LRU previous entry disappeared")
                .links
                .newer = Some(key.clone());
        } else {
            self.first = Some(key.clone());
        }
        entries
            .get_mut(key.as_str())
            .expect("cache LRU appended entry disappeared")
            .links = Links { older, newer: None };
        self.last = Some(key);
    }
    pub(super) fn remove(&mut self, entries: &mut Entries, url: &str) -> Option<Key> {
        let key = entries.get_key_value(url)?.0.clone();
        let links = std::mem::take(&mut entries.get_mut(url).unwrap().links);
        if let Some(previous) = &links.older {
            entries
                .get_mut(previous.as_str())
                .expect("cache LRU previous entry disappeared")
                .links
                .newer = links.newer.clone();
        } else {
            self.first = links.newer.clone();
        }
        if let Some(next) = &links.newer {
            entries
                .get_mut(next.as_str())
                .expect("cache LRU next entry disappeared")
                .links
                .older = links.older.clone();
        } else {
            self.last = links.older.clone();
        }
        Some(key)
    }
    pub(super) fn touch(&mut self, entries: &mut Entries, url: &str) {
        if let Some(key) = self.remove(entries, url) {
            self.append(entries, key);
        }
    }
    pub(super) fn pop(&mut self, entries: &mut Entries) -> Option<Key> {
        let first = self.first.clone()?;
        self.remove(entries, first.as_str())
    }
    pub(super) fn clear(&mut self) {
        self.first = None;
        self.last = None;
    }
    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.first.is_none() && self.last.is_none()
    }
    #[cfg(test)]
    pub(super) fn keys<'a>(&'a self, entries: &'a Entries) -> Keys<'a> {
        Keys {
            key: self.first.as_ref(),
            entries,
            remaining: entries.len(),
        }
    }
    #[cfg(test)]
    pub(super) fn valid(&self, entries: &Entries) -> bool {
        let mut key = self.first.as_ref();
        let mut older = None;
        let mut count = 0;
        while let Some(current) = key {
            if count >= entries.len() {
                return false;
            }
            let Some(entry) = entries.get(current.as_str()) else {
                return false;
            };
            if entry.links.older.as_ref() != older {
                return false;
            }
            older = Some(current);
            key = entry.links.newer.as_ref();
            count += 1;
        }
        count == entries.len() && older == self.last.as_ref()
    }
}
#[cfg(test)]
pub(super) struct Keys<'a> {
    key: Option<&'a Key>,
    entries: &'a Entries,
    remaining: usize,
}
#[cfg(test)]
impl<'a> Iterator for Keys<'a> {
    type Item = &'a Key;
    fn next(&mut self) -> Option<Self::Item> {
        let key = self.key.take()?;
        assert!(self.remaining > 0, "cache LRU cycle");
        self.remaining -= 1;
        self.key = self
            .entries
            .get(key.as_str())
            .expect("cache LRU entry disappeared")
            .links
            .newer
            .as_ref();
        Some(key)
    }
}

#[cfg(test)]
#[path = "cache_lru_tests.rs"]
mod tests;
