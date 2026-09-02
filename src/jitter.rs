use std::collections::VecDeque;

#[derive(Debug)]
struct BufferedBlock {
    samples: Vec<f32>,
    offset: usize,
    insertion_index: u64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EvictionStats {
    pub capacity_frames: u64,
    pub age_frames: u64,
}

#[derive(Debug, PartialEq)]
pub struct TakenAudio {
    pub samples: Vec<f32>,
    pub oldest_insertion_index: u64,
}

#[derive(Debug)]
pub struct JitterBuffer {
    blocks: VecDeque<BufferedBlock>,
    occupancy_frames: usize,
    maximum_frames: usize,
    maximum_age_periods: u64,
    evictions: EvictionStats,
}

impl JitterBuffer {
    pub fn new(maximum_frames: usize, maximum_age_periods: u64) -> Self {
        Self {
            blocks: VecDeque::new(),
            occupancy_frames: 0,
            maximum_frames,
            maximum_age_periods,
            evictions: EvictionStats::default(),
        }
    }

    pub fn occupancy(&self) -> usize {
        self.occupancy_frames
    }

    pub fn update_limits(
        &mut self,
        maximum_frames: usize,
        maximum_age_periods: u64,
        current_index: u64,
    ) {
        self.maximum_frames = maximum_frames;
        self.maximum_age_periods = maximum_age_periods;
        self.evict_aged(current_index);
        self.evict_capacity();
    }

    pub fn insert(&mut self, samples: Vec<f32>, insertion_index: u64) {
        self.evict_aged(insertion_index);
        if !samples.is_empty() {
            self.occupancy_frames += samples.len();
            self.blocks.push_back(BufferedBlock {
                samples,
                offset: 0,
                insertion_index,
            });
        }
        self.evict_capacity();
    }

    pub fn take(&mut self, frames: usize, current_index: u64) -> Option<TakenAudio> {
        self.evict_aged(current_index);
        if frames == 0 || self.occupancy_frames < frames {
            return None;
        }

        let oldest_insertion_index = self.blocks.front()?.insertion_index;
        let mut samples = Vec::with_capacity(frames);
        while samples.len() < frames {
            let needed = frames - samples.len();
            let front = self.blocks.front_mut().expect("occupancy tracks blocks");
            let available = front.samples.len() - front.offset;
            let count = needed.min(available);
            samples.extend_from_slice(&front.samples[front.offset..front.offset + count]);
            front.offset += count;
            self.occupancy_frames -= count;
            if front.offset == front.samples.len() {
                self.blocks.pop_front();
            }
        }

        Some(TakenAudio {
            samples,
            oldest_insertion_index,
        })
    }

    pub fn clear(&mut self) {
        self.blocks.clear();
        self.occupancy_frames = 0;
    }

    pub fn eviction_stats(&self) -> EvictionStats {
        self.evictions
    }

    fn evict_aged(&mut self, current_index: u64) {
        while self.blocks.front().is_some_and(|block| {
            current_index.saturating_sub(block.insertion_index) > self.maximum_age_periods
        }) {
            let block = self.blocks.pop_front().expect("front was present");
            let frames = block.samples.len() - block.offset;
            self.occupancy_frames -= frames;
            self.evictions.age_frames += frames as u64;
        }
    }

    fn evict_capacity(&mut self) {
        let mut excess = self.occupancy_frames.saturating_sub(self.maximum_frames);
        while excess > 0 {
            let front = self
                .blocks
                .front_mut()
                .expect("positive occupancy has a block");
            let available = front.samples.len() - front.offset;
            let count = excess.min(available);
            front.offset += count;
            excess -= count;
            self.occupancy_frames -= count;
            self.evictions.capacity_frames += count as u64;
            if front.offset == front.samples.len() {
                self.blocks.pop_front();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::JitterBuffer;
    use proptest::prelude::*;

    #[test]
    fn capacity_evicts_exactly_from_the_oldest_end() {
        let mut buffer = JitterBuffer::new(5, 100);
        buffer.insert(vec![1.0, 2.0, 3.0], 0);
        buffer.insert(vec![4.0, 5.0, 6.0, 7.0], 1);

        let taken = buffer.take(5, 1).unwrap();
        assert_eq!(taken.samples, vec![3.0, 4.0, 5.0, 6.0, 7.0]);
        assert_eq!(buffer.eviction_stats().capacity_frames, 2);
    }

    #[test]
    fn underflow_is_atomic_and_preserves_partial_blocks() {
        let mut buffer = JitterBuffer::new(10, 100);
        buffer.insert(vec![1.0, 2.0, 3.0], 4);

        assert!(buffer.take(4, 4).is_none());
        assert_eq!(buffer.occupancy(), 3);
        assert_eq!(buffer.take(3, 4).unwrap().samples, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn insertion_evicts_aged_media_without_renderer_progress() {
        let mut buffer = JitterBuffer::new(100, 2);
        buffer.insert(vec![1.0, 2.0], 1);
        buffer.insert(vec![3.0], 4);

        assert_eq!(buffer.occupancy(), 1);
        assert_eq!(buffer.eviction_stats().age_frames, 2);
    }

    #[test]
    fn take_evicts_age_before_checking_occupancy() {
        let mut buffer = JitterBuffer::new(100, 2);
        buffer.insert(vec![1.0, 2.0], 1);
        buffer.insert(vec![3.0, 4.0], 3);

        assert!(buffer.take(3, 4).is_none());
        assert_eq!(buffer.occupancy(), 2);
        assert_eq!(buffer.take(2, 4).unwrap().samples, vec![3.0, 4.0]);
    }

    proptest! {
        #[test]
        fn arbitrary_insert_take_and_limit_interleavings_remain_bounded(
            operations in prop::collection::vec((0u8..3, 0usize..128, 0u8..8), 0..256)
        ) {
            let mut maximum = 64usize;
            let mut current_index = 0u64;
            let mut buffer = JitterBuffer::new(maximum, 20);

            for (kind, amount, advance) in operations {
                current_index = current_index.saturating_add(advance as u64);
                match kind {
                    0 => buffer.insert(vec![0.0; amount], current_index),
                    1 => { let _ = buffer.take(amount, current_index); }
                    _ => {
                        maximum = amount.max(1);
                        buffer.update_limits(maximum, 20, current_index);
                    }
                }
                prop_assert!(buffer.occupancy() <= maximum);
            }
        }
    }
}
