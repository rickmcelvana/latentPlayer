//! A rolling window of the samples most recently sent to the output device,
//! for the scope to draw.
//!
//! Phase 0 didn't need this: the whole track sat in one buffer, so "the last N
//! samples played" was a slice ending at the playback cursor. Streaming decode
//! (M2) throws that buffer away -- once a block has been played it is gone --
//! so the samples the scope wants have to be kept deliberately.
//!
//! Written by the audio callback and read by a render thread, which rules out
//! a `Mutex`: blocking the callback on a lock the render thread happens to
//! hold is a dropout. Instead each sample is an `AtomicU32` holding the f32's
//! bits, with the write position published separately.
//!
//! One honest limitation: this does not stop the writer lapping the reader
//! mid-read, so a window read while the writer wraps past it can contain a
//! seam between old and new samples. That's a deliberate trade -- the
//! alternative is making the audio callback wait. In practice the ring holds
//! far more audio than a frame takes to read (a ~170ms ring against a
//! microsecond-scale read at 60fps), and the consequence of losing the race is
//! one visually imperfect frame of a scope, not a glitch anyone can hear.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Interleaved samples, in the source's own channel layout (so a stereo track
/// gives the scope real L/R pairs to plot against each other).
pub struct ScopeRing {
    samples: Box<[std::sync::atomic::AtomicU32]>,
    /// Total samples ever written, not an index -- the modulo happens at read
    /// and write time. Monotonic makes "the last N" trivial to express.
    written: AtomicUsize,
    mask: usize,
}

impl ScopeRing {
    /// `capacity` is rounded up to a power of two so the wrap is a mask rather
    /// than a division on the audio thread.
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(2).next_power_of_two();
        let samples = (0..capacity)
            .map(|_| std::sync::atomic::AtomicU32::new(0))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            samples,
            written: AtomicUsize::new(0),
            mask: capacity - 1,
        }
    }

    pub fn capacity(&self) -> usize {
        self.samples.len()
    }

    /// Appends samples. Called from the audio callback: no locks, no
    /// allocation, no syscalls.
    pub fn push(&self, block: &[f32]) {
        let start = self.written.load(Ordering::Relaxed);
        for (i, &sample) in block.iter().enumerate() {
            self.samples[(start + i) & self.mask].store(sample.to_bits(), Ordering::Relaxed);
        }
        // Release: the samples above must be visible to any reader that sees
        // this new count.
        self.written.store(start + block.len(), Ordering::Release);
    }

    /// The most recent `n` samples, oldest first. Shorter than `n` early on,
    /// when less than that has been played.
    pub fn window(&self, n: usize) -> Vec<f32> {
        let end = self.written.load(Ordering::Acquire);
        let n = n.min(self.capacity()).min(end);
        (end - n..end)
            .map(|i| f32::from_bits(self.samples[i & self.mask].load(Ordering::Relaxed)))
            .collect()
    }

    /// Forgets everything written so far, so the scope doesn't draw a frame
    /// spliced together from either side of a seek.
    pub fn clear(&self) {
        self.written.store(0, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_rounds_up_to_a_power_of_two() {
        assert_eq!(ScopeRing::new(1000).capacity(), 1024);
        assert_eq!(ScopeRing::new(1024).capacity(), 1024);
    }

    #[test]
    fn returns_the_most_recent_samples_in_order() {
        let ring = ScopeRing::new(16);
        ring.push(&[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(ring.window(4), vec![1.0, 2.0, 3.0, 4.0]);
        assert_eq!(ring.window(2), vec![3.0, 4.0]);
    }

    #[test]
    fn asking_for_more_than_has_been_played_returns_what_there_is() {
        let ring = ScopeRing::new(16);
        assert!(ring.window(8).is_empty(), "nothing played yet");
        ring.push(&[1.0, 2.0]);
        assert_eq!(ring.window(8), vec![1.0, 2.0]);
    }

    #[test]
    fn old_samples_are_overwritten_once_it_wraps() {
        let ring = ScopeRing::new(8);
        for i in 0..20 {
            ring.push(&[i as f32]);
        }
        assert_eq!(
            ring.window(8),
            (12..20).map(|i| i as f32).collect::<Vec<_>>()
        );
        assert_eq!(
            ring.window(100).len(),
            8,
            "a window can never exceed the ring"
        );
    }

    #[test]
    fn clear_empties_it() {
        let ring = ScopeRing::new(8);
        ring.push(&[1.0, 2.0, 3.0]);
        ring.clear();
        assert!(ring.window(8).is_empty());
        ring.push(&[9.0]);
        assert_eq!(ring.window(8), vec![9.0]);
    }

    #[test]
    fn survives_a_concurrent_reader() {
        // Not a proof of correctness -- races don't work that way -- but it
        // does catch the obvious failures (panics, out-of-range indexing)
        // under real contention, which is how this will actually be used.
        use std::sync::Arc;

        let ring = Arc::new(ScopeRing::new(4096));
        let writer = {
            let ring = Arc::clone(&ring);
            std::thread::spawn(move || {
                let block = [0.5f32; 128];
                for _ in 0..2000 {
                    ring.push(&block);
                }
            })
        };
        for _ in 0..2000 {
            for sample in ring.window(2048) {
                assert!(
                    (0.0..=1.0).contains(&sample),
                    "read back a sample that was never written: {sample}"
                );
            }
        }
        writer.join().expect("writer thread should not panic");
    }
}
