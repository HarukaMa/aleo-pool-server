use std::{
    collections::HashMap,
    hash::Hash,
    time::{Duration, Instant},
};

pub struct Cache<K: Eq + Hash + Clone, V: Clone> {
    duration: Duration,
    instants: HashMap<K, Instant>,
    values: HashMap<K, V>,
}

impl<K: Eq + Hash + Clone, V: Clone> Cache<K, V> {
    pub fn new(duration: Duration) -> Self {
        Cache {
            duration,
            instants: Default::default(),
            values: Default::default(),
        }
    }

    pub fn get(&self, key: K) -> Option<V> {
        let instant = self.instants.get(&key)?;
        if instant.elapsed() > self.duration {
            return None;
        }
        self.values.get(&key).cloned()
    }

    pub fn set(&mut self, key: K, value: V) {
        self.values.insert(key.clone(), value);
        self.instants.insert(key, Instant::now());
    }
}
