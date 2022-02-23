use std::time::{Duration, Instant};

pub(crate) struct Cache<T: Clone> {
    duration: Duration,
    instant: Instant,
    value: Option<T>,
}

impl<T: Clone> Cache<T> {
    pub fn new(duration: Duration) -> Self {
        Cache {
            duration,
            instant: Instant::now(),
            value: None,
        }
    }

    pub fn get(&self) -> Option<T> {
        if self.instant.elapsed() > self.duration {
            return None;
        }
        self.value.clone()
    }

    pub fn set(&mut self, value: T) {
        self.value = Some(value);
        self.instant = Instant::now();
    }
}
