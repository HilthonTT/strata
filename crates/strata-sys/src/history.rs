use std::collections::VecDeque;

/// A fixed-size ring of samples, used for sparklines.
#[derive(Debug, Clone)]
pub struct History {
    samples: VecDeque<u64>,
    capacity: usize,
}

impl History {
    pub fn new(capacity: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn push(&mut self, value: u64) {
        if self.samples.len() == self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(value);
    }

    pub fn as_vec(&self) -> Vec<u64> {
        self.samples.iter().copied().collect()
    }

    pub fn last(&self) -> u64 {
        self.samples.back().copied().unwrap_or(0)
    }

    pub fn max(&self) -> u64 {
        self.samples.iter().copied().max().unwrap_or(0)
    }
}

impl Default for History {
    fn default() -> Self {
        Self::new(120)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_drops_oldest() {
        let mut h = History::new(3);
        (1..=5).for_each(|v| h.push(v));
        assert_eq!(h.as_vec(), vec![3, 4, 5]);
        assert_eq!(h.max(), 5);
    }
}
