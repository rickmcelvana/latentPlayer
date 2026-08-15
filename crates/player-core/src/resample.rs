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
use rubato::{Fft, FixedSync, Resampler};

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
