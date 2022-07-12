use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use tokio::sync::RwLock;

pub struct Speedometer {
    storage: RwLock<VecDeque<(Instant, u64)>>,
    interval: Duration,
    cached: bool,
    cache_interval: Option<Duration>,
    cache_instant: Option<Instant>,
    cache_value: f64,
}

impl Speedometer {
    pub fn init(interval: Duration) -> Self {
        Self {
            storage: RwLock::new(VecDeque::new()),
            interval,
            cached: false,
            cache_interval: None,
            cache_instant: None,
            cache_value: 0.0,
        }
    }

    pub fn init_with_cache(interval: Duration, cache_interval: Duration) -> Self {
        Self {
            storage: RwLock::new(VecDeque::new()),
            interval,
            cached: true,
            cache_interval: Some(cache_interval),
            cache_instant: Some(Instant::now() - cache_interval),
            cache_value: 0.0,
        }
    }

    pub async fn event(&self, value: u64) {
        let mut storage = self.storage.write().await;
        storage.push_back((Instant::now(), value));
        while storage.front().map_or(false, |t| t.0.elapsed() > self.interval) {
            storage.pop_front();
        }
    }

    pub async fn speed(&mut self) -> f64 {
        if self.cached && self.cache_instant.unwrap().elapsed() < self.cache_interval.unwrap() {
            return self.cache_value;
        }
        let mut storage = self.storage.write().await;
        while storage.front().map_or(false, |t| t.0.elapsed() > self.interval) {
            storage.pop_front();
        }
        drop(storage);
        let events = self.storage.read().await.iter().fold(0, |acc, t| acc + t.1);
        let speed = events as f64 / self.interval.as_secs_f64();
        if self.cached {
            self.cache_instant = Some(Instant::now());
            self.cache_value = speed;
        }
        speed
    }

    #[allow(dead_code)]
    pub async fn reset(&self) {
        self.storage.write().await.clear();
    }
}
