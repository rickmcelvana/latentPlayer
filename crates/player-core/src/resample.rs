//! Sample-rate conversion for the native output path.
//!
//! `playback.rs` always opens the output device at its own mix rate (see the
//! note there for why it has no choice), so the decoded audio has to be
//! converted to meet it. Phase 0 shipped linear interpolation as an
//! openly-labelled stopgap after a 44.1kHz file on a 96kHz device played at
//! 2.177x speed; this is Phase 1's real answer.
//!
//! Backed by `rubato`'s FFT-based synchronous resampler. That's the right
//! family for this job: the ratio is fixed and rational (44100 -> 48000 and
//! friends), which is exactly what a synchronous resampler is for, and it
//! applies a proper anti-aliasing window rather than the nothing-at-all that
//! linear interpolation offered when *down*sampling.
//!
//! Native-only, for the same reason `cpal` is: the web build runs its
//! `AudioContext` at the file's own rate and has nothing to convert.
//!
//! Built against rubato 5.0.0's real API, verified against its source rather
//! than written from memory -- 5.x moved buffers to the `audioadapter`
//! traits and renamed the resamplers (`Fft`/`FixedSync`, `Async`/
//! `FixedAsync`), so the `SincFixedIn`/`Vec<Vec<f32>>` shape in most
//! examples online no longer exists.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler};

use crate::error::PlayerError;

/// Frames per processing chunk. rubato picks its FFT sub-chunk size from
/// this (targeting ~256 frames); 1024 keeps that comfortably in range.
const CHUNK_FRAMES: usize = 1024;

/// Resamples interleaved `channels`-channel audio from `from_rate` to
/// `to_rate`.
///
/// Returns `samples` untouched when the rates already match -- the common
/// "device happens to run at the file's rate" case then costs nothing and,
/// more importantly, stays bit-exact instead of being run through a filter
/// for no reason.
pub fn resample_interleaved(
    samples: Vec<f32>,
    channels: u16,
    from_rate: u32,
    to_rate: u32,
) -> Result<Vec<f32>, PlayerError> {
    let ch = channels.max(1) as usize;
    let in_frames = samples.len() / ch;
    if from_rate == to_rate || from_rate == 0 || to_rate == 0 || in_frames == 0 {
        return Ok(samples);
    }

    let mut resampler = Fft::<f32>::new(
        from_rate as usize,
        to_rate as usize,
        CHUNK_FRAMES,
        ch,
        FixedSync::Both,
    )
    .map_err(|e| PlayerError::Resample(e.to_string()))?;

    let input = InterleavedSlice::new(&samples, ch, in_frames)
        .map_err(|e| PlayerError::Resample(e.to_string()))?;

    // `process_all` handles chunking, the final partial chunk, and trimming
    // the resampler's own start-up delay, then truncates to the frames it
    // actually produced. Doing that by hand is the classic place to leak a
    // few frames of silence onto the front of every track.
    let output = resampler
        .process_all(&input, in_frames, None)
        .map_err(|e| PlayerError::Resample(e.to_string()))?;

    Ok(output.take_data())
}

/// The same conversion, driven one decoded block at a time.
///
/// `resample_interleaved` above needs the whole clip in memory, which is
/// exactly what M2's streaming decode exists to stop doing. This is the
/// streaming form: feed it blocks as they come out of `StreamingDecoder`,
/// take resampled blocks back, call `finish` at end of stream, `reset` on a
/// seek.
///
/// Three details that `process_all` used to handle and now have to be handled
/// here -- they are the classic ways a hand-rolled streaming resampler goes
/// wrong:
///
/// 1. **Startup delay.** The filter reports `output_delay()` output frames of
///    latency; left in, every track (and every seek) starts with a sliver of
///    silence and runs permanently late. Those frames are dropped as they
///    emerge.
/// 2. **The final partial chunk.** The resampler wants a fixed number of input
///    frames per call, and a track almost never ends on that boundary.
///    `finish` pads the remainder with silence and tells rubato how much of it
///    was real via `Indexing::partial_len`.
/// 3. **The tail.** Audio still inside the filter at end of stream only comes
///    out if more input is pushed through, so `finish` pumps zeros until the
///    output length matches the ratio. Skipping this truncates every track by
///    roughly the filter length.
pub struct StreamingResampler {
    /// `None` when the rates already match: a pass-through, bit-exact and
    /// free, same as `resample_interleaved`'s early return.
    resampler: Option<Fft<f32>>,
    channels: usize,
    /// Kept as the two integer rates rather than as a ratio: the output length
    /// is computed from them at end of stream, and `ceil(44100 * (48000 /
    /// 44100))` in f64 is 48001, not 48000. `process_all` has that same
    /// rounding artefact; here it would put a stray frame of filter tail at
    /// the end of every track, i.e. exactly in M3's gapless seam.
    from_rate: u64,
    to_rate: u64,
    /// Input frames accepted but not yet consumed by a full chunk.
    pending: Vec<f32>,
    /// Output scratch, sized once to the resampler's maximum chunk.
    scratch: Vec<f32>,
    /// Silence, for padding the final partial chunk and pumping the tail.
    silence: Vec<f32>,
    /// Startup-delay frames still to be dropped.
    delay_left: usize,
    in_frames: u64,
    out_frames: u64,
}

impl StreamingResampler {
    pub fn new(channels: u16, from_rate: u32, to_rate: u32) -> Result<Self, PlayerError> {
        let channels = channels.max(1) as usize;
        if from_rate == to_rate || from_rate == 0 || to_rate == 0 {
            return Ok(Self {
                resampler: None,
                channels,
                from_rate: 1,
                to_rate: 1,
                pending: Vec::new(),
                scratch: Vec::new(),
                silence: Vec::new(),
                delay_left: 0,
                in_frames: 0,
                out_frames: 0,
            });
        }

        let resampler = Fft::<f32>::new(
            from_rate as usize,
            to_rate as usize,
            CHUNK_FRAMES,
            channels,
            FixedSync::Both,
        )
        .map_err(|e| PlayerError::Resample(e.to_string()))?;

        let scratch = vec![0.0; resampler.output_frames_max() * channels];
        let silence = vec![0.0; resampler.input_frames_max() * channels];
        let delay_left = resampler.output_delay();

        Ok(Self {
            resampler: Some(resampler),
            channels,
            from_rate: from_rate as u64,
            to_rate: to_rate as u64,
            pending: Vec::new(),
            scratch,
            silence,
            delay_left,
            in_frames: 0,
            out_frames: 0,
        })
    }

    /// True when input passes straight through untouched.
    pub fn is_passthrough(&self) -> bool {
        self.resampler.is_none()
    }

    /// Feeds one block of interleaved input, appending whatever comes out to
    /// `out`. Output lags input by up to one chunk, so a call producing
    /// nothing is normal rather than an error.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), PlayerError> {
        let Some(resampler) = self.resampler.as_mut() else {
            out.extend_from_slice(input);
            return Ok(());
        };

        let channels = self.channels;
        self.pending.extend_from_slice(input);
        self.in_frames += (input.len() / channels) as u64;

        // Destructured so the input adapter (borrowing `pending`) and the
        // output adapter (borrowing `scratch`) can be alive at once.
        let Self {
            pending,
            scratch,
            delay_left,
            out_frames,
            ..
        } = self;

        loop {
            let need = resampler.input_frames_next();
            if pending.len() / channels < need {
                return Ok(());
            }

            let produced = run_chunk(
                resampler,
                &pending[..need * channels],
                scratch,
                channels,
                None,
            )?;
            emit(
                &scratch[..produced * channels],
                channels,
                delay_left,
                out_frames,
                out,
            );
            pending.drain(..need * channels);
        }
    }

    /// Flushes the last partial chunk and the filter's tail. Call once, at end
    /// of stream; `process` after this returns nothing useful until `reset`.
    pub fn finish(&mut self, out: &mut Vec<f32>) -> Result<(), PlayerError> {
        let Some(resampler) = self.resampler.as_mut() else {
            return Ok(());
        };

        let channels = self.channels;
        let expected = (self.in_frames * self.to_rate).div_ceil(self.from_rate);
        let start_len = out.len();

        let Self {
            pending,
            scratch,
            silence,
            delay_left,
            out_frames,
            ..
        } = self;

        // The real remainder, zero-padded up to a full chunk. `partial_len`
        // tells rubato how much of that is audio and how much is padding.
        if !pending.is_empty() {
            let real_frames = pending.len() / channels;
            let need = resampler.input_frames_next();
            pending.resize(need * channels, 0.0);
            let produced = run_chunk(
                resampler,
                &pending[..need * channels],
                scratch,
                channels,
                Some(real_frames),
            )?;
            emit(
                &scratch[..produced * channels],
                channels,
                delay_left,
                out_frames,
                out,
            );
            pending.clear();
        }

        // Pump silence until as much audio has come out as went in. Bounded
        // rather than `while`: a resampler that somehow never reaches the
        // expected length must not hang the decoder thread.
        let max_pumps = 64;
        for _ in 0..max_pumps {
            if *out_frames >= expected {
                break;
            }
            let need = resampler.input_frames_next();
            let produced = run_chunk(
                resampler,
                &silence[..need * channels],
                scratch,
                channels,
                Some(0),
            )?;
            emit(
                &scratch[..produced * channels],
                channels,
                delay_left,
                out_frames,
                out,
            );
        }

        // Trim the overshoot from the last pump so a track's resampled length
        // is exactly its source length times the ratio.
        if *out_frames > expected {
            let extra = ((*out_frames - expected) as usize * channels).min(out.len() - start_len);
            out.truncate(out.len() - extra);
            *out_frames = expected;
        }

        Ok(())
    }

    /// Drops all filter state, as a seek requires: the samples either side of
    /// a seek are unrelated, so carrying history across one smears audio from
    /// the old position into the new. Re-arms the startup-delay trim, since
    /// the filter restarts from silence.
    pub fn reset(&mut self) {
        if let Some(resampler) = self.resampler.as_mut() {
            resampler.reset();
            self.delay_left = resampler.output_delay();
        }
        self.pending.clear();
        self.in_frames = 0;
        self.out_frames = 0;
    }
}

/// One `process_into_buffer` call, returning the output frames produced.
fn run_chunk(
    resampler: &mut Fft<f32>,
    input: &[f32],
    scratch: &mut [f32],
    channels: usize,
    partial_len: Option<usize>,
) -> Result<usize, PlayerError> {
    let in_frames = input.len() / channels;
    let out_frames = scratch.len() / channels;

    let adapter_in = InterleavedSlice::new(input, channels, in_frames)
        .map_err(|e| PlayerError::Resample(e.to_string()))?;
    let mut adapter_out = InterleavedSlice::new_mut(scratch, channels, out_frames)
        .map_err(|e| PlayerError::Resample(e.to_string()))?;

    let indexing = Indexing {
        input_offset: 0,
        output_offset: 0,
        partial_len,
        active_channels_mask: None,
    };

    let (_consumed, produced) = resampler
        .process_into_buffer(&adapter_in, &mut adapter_out, Some(&indexing))
        .map_err(|e| PlayerError::Resample(e.to_string()))?;
    Ok(produced)
}

/// Appends resampled output, dropping any startup-delay frames still owed.
fn emit(
    produced: &[f32],
    channels: usize,
    delay_left: &mut usize,
    out_frames: &mut u64,
    out: &mut Vec<f32>,
) {
    let frames = produced.len() / channels;
    let skip = (*delay_left).min(frames);
    *delay_left -= skip;
    out.extend_from_slice(&produced[skip * channels..]);
    *out_frames += (frames - skip) as u64;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frames in an interleaved buffer.
    fn frames(buf: &[f32], channels: usize) -> usize {
        buf.len() / channels
    }

    #[test]
    fn identical_rates_pass_through_untouched() {
        let input = vec![0.1, 0.2, 0.3, 0.4];
        let out = resample_interleaved(input.clone(), 2, 48_000, 48_000).unwrap();
        assert_eq!(out, input, "matching rates must stay bit-exact");
    }

    #[test]
    fn upsampling_stretches_frame_count_by_the_rate_ratio() {
        let in_frames = 44_100;
        let input = vec![0.0f32; in_frames * 2];
        let out = resample_interleaved(input, 2, 44_100, 48_000).unwrap();
        let out_frames = frames(&out, 2);
        let expected = 48_000;
        // A real resampler works in chunks, so the tail lands within a chunk
        // of the ideal length rather than exactly on it.
        assert!(
            out_frames.abs_diff(expected) <= CHUNK_FRAMES,
            "expected ~{expected} frames, got {out_frames}"
        );
    }

    #[test]
    fn a_sine_keeps_its_frequency_in_wall_clock_time() {
        // The bug this module exists to prevent: one second of 440Hz must
        // still be one second of 440Hz, not 2.177 seconds' worth crammed
        // into one.
        let (from, to) = (44_100u32, 96_000u32);
        let input: Vec<f32> = (0..from)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / from as f32).sin())
            .collect();
        let out = resample_interleaved(input, 1, from, to).unwrap();

        assert!(
            frames(&out, 1).abs_diff(to as usize) <= CHUNK_FRAMES,
            "one second in should be one second out, got {} frames",
            out.len()
        );

        // 440Hz for one second crosses zero going positive ~440 times,
        // whatever the sample rate. Skip the edges, where any windowed
        // resampler rings.
        let core = &out[4096..out.len() - 4096];
        let crossings = core
            .windows(2)
            .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
            .count();
        let expected = (440.0 * core.len() as f64 / to as f64).round() as usize;
        assert!(
            crossings.abs_diff(expected) <= 2,
            "expected ~{expected} positive-going crossings, got {crossings}"
        );
    }

    #[test]
    fn a_resampled_sine_stays_a_clean_sine() {
        // Frequency alone doesn't prove quality -- linear interpolation
        // passed that too, while adding audible harmonics and aliasing.
        // This measures what's left *after* the 440Hz tone: harmonics,
        // aliases, and noise. That's where a real resampler earns its keep.
        //
        // Deliberately phase-independent. Comparing sample-by-sample against
        // an analytic sine instead measures the resampler's constant group
        // delay, not its quality: rubato leaves a residual ~half-sample
        // offset here, which caps a sample-wise SNR around 31dB no matter
        // how clean the waveform actually is (measured, not guessed).
        let (from, to) = (44_100u32, 48_000u32);
        let input: Vec<f32> = (0..from)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / from as f32).sin())
            .collect();
        let out = resample_interleaved(input, 1, from, to).unwrap();

        // Trim the edges, where any windowed resampler rings against the
        // clip boundary, then analyse a whole number of 1200-sample blocks:
        // 440Hz completes exactly 11 cycles per 1200 samples at 48kHz, so
        // the tone lands exactly on a DFT bin and contributes no leakage.
        let core = &out[8192..out.len() - 8192];
        let usable = (core.len() / 1200) * 1200;
        let core = &core[..usable];
        assert!(usable > 0, "not enough output to analyse");

        let n = usable as f64;
        let (mut re, mut im, mut total) = (0.0f64, 0.0f64, 0.0f64);
        for (i, &x) in core.iter().enumerate() {
            let phase = 2.0 * std::f64::consts::PI * 440.0 * i as f64 / to as f64;
            re += x as f64 * phase.cos();
            im += x as f64 * phase.sin();
            total += (x as f64) * (x as f64);
        }
        // re/im are sums, so the tone's mean power is 2(re^2+im^2)/n^2.
        let tone = 2.0 * (re * re + im * im) / (n * n);
        let mean_power = total / n;
        let rest = (mean_power - tone).max(f64::MIN_POSITIVE);
        let purity_db = 10.0 * (tone / rest).log10();

        // Measured at ~85dB on rubato's FFT resampler. 70 leaves headroom
        // for platform FFT differences while still being far out of reach
        // of the linear interpolation this replaced.
        assert!(
            purity_db > 70.0,
            "resampled tone should be clean; tone-vs-everything-else was {purity_db:.1} dB"
        );
    }

    /// Runs a clip through the streaming resampler in `block` frame chunks,
    /// the way the decoder thread feeds it.
    fn stream(input: &[f32], channels: u16, from: u32, to: u32, block: usize) -> Vec<f32> {
        let mut resampler = StreamingResampler::new(channels, from, to).unwrap();
        let mut out = Vec::new();
        for chunk in input.chunks(block * channels as usize) {
            resampler.process(chunk, &mut out).unwrap();
        }
        resampler.finish(&mut out).unwrap();
        out
    }

    #[test]
    fn streaming_matches_the_whole_buffer_resampler() {
        // The guarantee that makes the streaming path safe to adopt: the same
        // audio comes out either way, so M1's measured ~85dB quality carries
        // over unchanged rather than needing to be re-argued.
        let input: Vec<f32> = (0..44_100)
            .flat_map(|i| {
                let t = i as f32 / 44_100.0;
                [
                    (2.0 * std::f32::consts::PI * 440.0 * t).sin(),
                    (2.0 * std::f32::consts::PI * 997.0 * t).sin(),
                ]
            })
            .collect();

        let whole = resample_interleaved(input.clone(), 2, 44_100, 48_000).unwrap();
        let streamed = stream(&input, 2, 44_100, 48_000, 1152);

        assert_eq!(
            streamed,
            whole[..streamed.len()],
            "streamed resampling must be bit-exact against the whole-buffer path"
        );
        // The two disagree only on where to stop: `process_all` sizes its
        // output with `ceil(len * ratio)` in floating point, which returns
        // 48001 for a second of 44.1 -> 48kHz. The streaming path computes
        // that length from the integer rates instead, so it stops one frame of
        // filter tail earlier. See `from_rate`/`to_rate` for why that matters.
        assert!(
            whole.len() - streamed.len() <= 2,
            "lengths should differ by at most one frame, got {} vs {}",
            whole.len(),
            streamed.len()
        );
    }

    #[test]
    fn block_size_does_not_change_the_result() {
        // The decoder hands over whatever a packet decodes to -- 1152 frames
        // for MP3, 1024 for AAC, 4096 for FLAC -- so the output must not
        // depend on how the input happens to be chopped up.
        let input: Vec<f32> = (0..20_000)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 44_100.0).sin())
            .collect();

        let reference = stream(&input, 1, 44_100, 48_000, 1152);
        for block in [1, 97, 1024, 4096, 20_000] {
            let out = stream(&input, 1, 44_100, 48_000, block);
            assert_eq!(
                out, reference,
                "block size {block} changed the resampled output"
            );
        }
    }

    #[test]
    fn streaming_preserves_clip_length() {
        let input = vec![0.0f32; 44_100 * 2];
        let out = stream(&input, 2, 44_100, 48_000, 1024);
        assert_eq!(
            frames(&out, 2),
            48_000,
            "one second in must be exactly one second out"
        );
    }

    #[test]
    fn matching_rates_stream_through_untouched() {
        let input: Vec<f32> = (0..1000).map(|i| i as f32 / 1000.0).collect();
        let resampler = StreamingResampler::new(2, 48_000, 48_000).unwrap();
        assert!(resampler.is_passthrough());
        let out = stream(&input, 2, 48_000, 48_000, 128);
        assert_eq!(out, input, "matching rates must stay bit-exact");
    }

    #[test]
    fn reset_clears_state_so_a_seek_starts_clean() {
        // A resampler that kept its history across a seek would smear audio
        // from the old position into the new one. Feeding loud audio, then
        // resetting and feeding silence, must produce silence.
        let loud = vec![0.9f32; 8192];
        let quiet = vec![0.0f32; 8192];

        let mut resampler = StreamingResampler::new(1, 44_100, 48_000).unwrap();
        let mut sink = Vec::new();
        resampler.process(&loud, &mut sink).unwrap();
        resampler.reset();

        let mut out = Vec::new();
        resampler.process(&quiet, &mut out).unwrap();
        resampler.finish(&mut out).unwrap();

        let worst = out.iter().fold(0.0f32, |acc, s| acc.max(s.abs()));
        assert!(
            worst < 1e-6,
            "state leaked across reset; loudest sample after it was {worst}"
        );
    }

    #[test]
    fn the_start_of_the_stream_is_not_silence() {
        // The startup-delay trim, checked directly: without it every track
        // would open with ~a filter length of silence and run late.
        let input = vec![0.5f32; 44_100];
        let out = stream(&input, 1, 44_100, 48_000, 1024);
        assert!(
            out[..64].iter().any(|s| s.abs() > 0.1),
            "output starts with silence -- the startup delay was not trimmed"
        );
    }

    #[test]
    fn channels_stay_independent() {
        // A resampler that mixed channels would pull the constant right
        // channel away from 1.0 as the left one ramps.
        let in_frames = 8192;
        let mut input = Vec::with_capacity(in_frames * 2);
        for i in 0..in_frames {
            input.push((i as f32 / in_frames as f32) * 2.0 - 1.0);
            input.push(1.0);
        }
        let out = resample_interleaved(input, 2, 44_100, 48_000).unwrap();

        // Skip the edges; a windowed resampler rings against the
        // discontinuity at the clip boundary.
        let out_frames = frames(&out, 2);
        for frame in 2048..out_frames - 2048 {
            let right = out[frame * 2 + 1];
            assert!(
                (right - 1.0).abs() < 0.01,
                "right channel drifted to {right} at frame {frame}"
            );
        }
    }
}
