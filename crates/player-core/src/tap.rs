//! The analysis tap: a rolling window of the samples most recently sent to the
//! output device, which the scope draws and every meter reads.
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
//! # Two ways to read it (M4)
//!
//! - [`AnalysisTap::window`] -- "the last N samples", for anything drawing a
//!   snapshot: the scope, and the spectrum's FFT frame. Missing samples between
//!   one call and the next is correct behaviour here; a display only ever wants
//!   what is on screen now.
//! - [`TapCursor`] -- a reader position, for anything that has to see *every*
//!   sample it claims to have measured. M5's integrated LUFS is the case that
//!   forces this: a number that says "this track measured -14 LUFS" is a lie if
//!   the meter silently skipped a second of it. The cursor cannot stop the
//!   writer lapping it, but it can say that it happened, which is the
//!   difference between a meter that degrades honestly and one that doesn't.
//!
//! One honest limitation, unchanged: this does not stop the writer lapping the
//! reader mid-read, so a window read while the writer wraps past it can contain
//! a seam between old and new samples. That's a deliberate trade -- the
//! alternative is making the audio callback wait. In practice the ring holds
//! far more audio than a frame takes to read (a ~170ms ring against a
//! microsecond-scale read at 60fps), and the consequence of losing the race is
//! one visually imperfect frame of a scope, not a glitch anyone can hear.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Interleaved samples, in the output device's channel layout (M3 moved
/// channel adaptation ahead of the audio ring, so this is what the device
/// actually received).
pub struct AnalysisTap {
    samples: Box<[std::sync::atomic::AtomicU32]>,
    /// Total samples ever written, not an index -- the modulo happens at read
    /// and write time. Monotonic makes "the last N" trivial to express.
    written: AtomicUsize,
    mask: usize,
}

/// A reader's position in the tap, in total-samples-written terms.
///
/// Separate from the tap itself so several readers can sit at different
/// positions without knowing about each other -- the spectrum and (from M5)
/// the loudness meter drain at different rates.
pub struct TapCursor {
    at: usize,
}

/// What one [`AnalysisTap::read`] produced.
pub struct TapRead {
    /// Samples appended to the caller's buffer.
    pub read: usize,
    /// The stream this reader is following broke: either the writer lapped the
    /// cursor (samples were overwritten before being read) or the tap was
    /// cleared under it by a seek. Either way the audio just handed over does
    /// not continue the audio handed over last time, so a meter holding state
    /// across calls has to reset rather than carry it into a different moment
    /// of the track.
    pub discontinuity: bool,
}

impl TapCursor {
    /// Starts at the tap's current write position, i.e. reading from now on
    /// rather than from whatever history happens to be in the ring.
    pub fn new(tap: &AnalysisTap) -> Self {
        Self {
            at: tap.written.load(Ordering::Acquire),
        }
    }
}

impl AnalysisTap {
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

    /// Appends `n` samples of silence -- what the callback sends the device
    /// when the ring has run dry.
    ///
    /// Recording this matters more than it looks. Without it the scope only
    /// ever sees samples that *arrived*, so a dropout leaves the trace frozen
    /// on the last good audio instead of showing the hole: the one moment the
    /// scope is most useful is the one it used to hide. It also made a live
    /// test of the gapless seam vacuous -- stalling the decoder for three
    /// seconds produced no measurable silence at all (found 2026-08-15).
    pub fn push_silence(&self, n: usize) {
        let start = self.written.load(Ordering::Relaxed);
        for i in 0..n {
            self.samples[(start + i) & self.mask].store(0f32.to_bits(), Ordering::Relaxed);
        }
        self.written.store(start + n, Ordering::Release);
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

    /// Appends everything written since `cursor` last read, advancing it.
    ///
    /// Bounded by the ring: a reader that has been away for longer than the tap
    /// holds gets the ring's worth that survived, with `discontinuity` set. It
    /// is deliberately not an error -- the audio thread must never wait for a
    /// meter, so falling behind has to be survivable and *visible*, in that
    /// order.
    pub fn read(&self, cursor: &mut TapCursor, out: &mut Vec<f32>) -> TapRead {
        let end = self.written.load(Ordering::Acquire);
        // `clear` (a seek) resets the count, leaving the cursor ahead of the
        // writer. Nothing was lost, but what comes next is from somewhere else
        // in the track, which is the same problem for a meter.
        let mut discontinuity = cursor.at > end;
        let mut start = if discontinuity { 0 } else { cursor.at };
        // Lapped: the oldest sample still in the ring is newer than the cursor.
        if end - start > self.capacity() {
            start = end - self.capacity();
            discontinuity = true;
        }
        out.extend(
            (start..end)
                .map(|i| f32::from_bits(self.samples[i & self.mask].load(Ordering::Relaxed))),
        );
        cursor.at = end;
        TapRead {
            read: end - start,
            discontinuity,
        }
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
        assert_eq!(AnalysisTap::new(1000).capacity(), 1024);
        assert_eq!(AnalysisTap::new(1024).capacity(), 1024);
    }

    #[test]
    fn returns_the_most_recent_samples_in_order() {
        let ring = AnalysisTap::new(16);
        ring.push(&[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(ring.window(4), vec![1.0, 2.0, 3.0, 4.0]);
        assert_eq!(ring.window(2), vec![3.0, 4.0]);
    }

    #[test]
    fn asking_for_more_than_has_been_played_returns_what_there_is() {
        let ring = AnalysisTap::new(16);
        assert!(ring.window(8).is_empty(), "nothing played yet");
        ring.push(&[1.0, 2.0]);
        assert_eq!(ring.window(8), vec![1.0, 2.0]);
    }

    #[test]
    fn old_samples_are_overwritten_once_it_wraps() {
        let ring = AnalysisTap::new(8);
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
    fn silence_is_recorded_like_any_other_audio() {
        let ring = AnalysisTap::new(16);
        ring.push(&[1.0, 2.0]);
        ring.push_silence(3);
        ring.push(&[3.0]);
        assert_eq!(ring.window(6), vec![1.0, 2.0, 0.0, 0.0, 0.0, 3.0]);
    }

    #[test]
    fn clear_empties_it() {
        let ring = AnalysisTap::new(8);
        ring.push(&[1.0, 2.0, 3.0]);
        ring.clear();
        assert!(ring.window(8).is_empty());
        ring.push(&[9.0]);
        assert_eq!(ring.window(8), vec![9.0]);
    }

    #[test]
    fn a_cursor_reads_every_sample_written_since_last_time() {
        let tap = AnalysisTap::new(16);
        let mut cursor = TapCursor::new(&tap);
        let mut out = Vec::new();

        tap.push(&[1.0, 2.0]);
        let first = tap.read(&mut cursor, &mut out);
        assert_eq!(out, vec![1.0, 2.0]);
        assert_eq!(first.read, 2);
        assert!(!first.discontinuity);

        // Appends rather than replaces, and picks up exactly where it stopped.
        tap.push(&[3.0]);
        tap.push(&[4.0]);
        let second = tap.read(&mut cursor, &mut out);
        assert_eq!(out, vec![1.0, 2.0, 3.0, 4.0]);
        assert_eq!(second.read, 2);
        assert!(!second.discontinuity);

        // Nothing new is not a discontinuity, just nothing new.
        out.clear();
        let idle = tap.read(&mut cursor, &mut out);
        assert_eq!(idle.read, 0);
        assert!(!idle.discontinuity);
        assert!(out.is_empty());
    }

    #[test]
    fn a_new_cursor_starts_at_now_rather_than_replaying_history() {
        let tap = AnalysisTap::new(16);
        tap.push(&[1.0, 2.0, 3.0]);

        let mut cursor = TapCursor::new(&tap);
        let mut out = Vec::new();
        assert_eq!(tap.read(&mut cursor, &mut out).read, 0);

        tap.push(&[4.0]);
        tap.read(&mut cursor, &mut out);
        assert_eq!(
            out,
            vec![4.0],
            "history before the cursor existed is not its"
        );
    }

    #[test]
    fn being_lapped_is_reported_rather_than_hidden() {
        let tap = AnalysisTap::new(8);
        let mut cursor = TapCursor::new(&tap);
        let mut out = Vec::new();

        // Twice the ring, without reading in between.
        for i in 0..16 {
            tap.push(&[i as f32]);
        }
        let read = tap.read(&mut cursor, &mut out);
        assert!(read.discontinuity, "8 samples of ring, 16 written");
        assert_eq!(read.read, 8);
        assert_eq!(
            out,
            (8..16).map(|i| i as f32).collect::<Vec<_>>(),
            "what survived is the newest ring's worth"
        );

        // Caught up again, so the next read is continuous.
        tap.push(&[99.0]);
        out.clear();
        let caught_up = tap.read(&mut cursor, &mut out);
        assert!(!caught_up.discontinuity);
        assert_eq!(out, vec![99.0]);
    }

    #[test]
    fn a_clear_under_a_reader_is_a_discontinuity() {
        // What a seek does: the samples after it are from somewhere else in the
        // track, so a meter carrying state across the boundary would be
        // measuring two different moments as one.
        let tap = AnalysisTap::new(16);
        let mut cursor = TapCursor::new(&tap);
        let mut out = Vec::new();
        tap.push(&[1.0, 2.0, 3.0]);
        tap.read(&mut cursor, &mut out);

        tap.clear();
        tap.push(&[9.0]);
        out.clear();
        let after = tap.read(&mut cursor, &mut out);
        assert!(after.discontinuity);
        assert_eq!(out, vec![9.0]);
    }

    #[test]
    fn survives_a_concurrent_reader() {
        // Not a proof of correctness -- races don't work that way -- but it
        // does catch the obvious failures (panics, out-of-range indexing)
        // under real contention, which is how this will actually be used.
        use std::sync::Arc;

        let ring = Arc::new(AnalysisTap::new(4096));
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
