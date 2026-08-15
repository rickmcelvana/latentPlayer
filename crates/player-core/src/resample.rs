//! Sample-rate conversion for the native output path.
//!
//! Phase 0 deliberately had none of this, and it broke the first Windows
//! desktop test (2026-08-15): a 44.1kHz MP3 fed to a device running at
//! 96kHz played at 2.177x speed. `playback.rs` now always opens the device
//! at its own mix rate (see the note there for why it has no choice), so
//! something has to bridge the file's rate to the device's, and this is it.
//!
//! **This is linear interpolation, not a real resampler.** It is honest
//! about pitch/duration -- which is the entire bug it exists to fix -- but
//! it is not transparent: interpolating between two neighbouring samples
//! rolls off the top octave slightly when upsampling, and offers no
//! anti-alias filtering at all when *down*sampling (a 96kHz file played on
//! a 48kHz device will fold anything above 24kHz back into the audible
//! band). For a walking skeleton playing 44.1kHz material on 48/96kHz
//! hardware -- i.e. upsampling, the common case -- that is inaudible in
//! practice. A proper windowed-sinc/polyphase resampler (`rubato`) is
//! Phase 1 work, and belongs here when the metering suite lands, since
//! LUFS/true-peak numbers computed off this would be subtly wrong.

/// Resamples interleaved `channels`-channel audio from `from_rate` to
/// `to_rate`. Returns `samples` untouched when the rates already match, so
/// the common "device happens to run at the file's rate" case costs
/// nothing.
pub fn resample_interleaved(
    samples: Vec<f32>,
    channels: u16,
    from_rate: u32,
    to_rate: u32,
) -> Vec<f32> {
    let ch = channels.max(1) as usize;
    let in_frames = samples.len() / ch;
    if from_rate == to_rate || from_rate == 0 || to_rate == 0 || in_frames < 2 {
        return samples;
    }

    let ratio = to_rate as f64 / from_rate as f64;
    let out_frames = ((in_frames as f64) * ratio).round() as usize;
    let mut out = vec![0.0f32; out_frames * ch];

    for i in 0..out_frames {
        // Position in *input* frames that this output frame samples from.
        let src = i as f64 / ratio;
        let i0 = src.floor() as usize;
        if i0 >= in_frames {
            break;
        }
        let i1 = (i0 + 1).min(in_frames - 1);
        let frac = (src - i0 as f64) as f32;
        for c in 0..ch {
            let a = samples[i0 * ch + c];
            let b = samples[i1 * ch + c];
            out[i * ch + c] = a + (b - a) * frac;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_rates_pass_through_untouched() {
        let input = vec![0.1, 0.2, 0.3, 0.4];
        assert_eq!(
            resample_interleaved(input.clone(), 2, 48_000, 48_000),
            input
        );
    }

    #[test]
    fn upsampling_stretches_frame_count_by_the_rate_ratio() {
        // 100 stereo frames at 44.1k -> ~108.8 frames at 48k.
        let input = vec![0.5f32; 100 * 2];
        let out = resample_interleaved(input, 2, 44_100, 48_000);
        let out_frames = out.len() / 2;
        let expected = (100.0 * 48_000.0 / 44_100.0f64).round() as usize;
        assert_eq!(out_frames, expected);
    }

    #[test]
    fn channels_stay_independent() {
        // L ramps up, R is constant -- a resampler that mixed the two up
        // would show R drifting away from 1.0.
        let frames = 64;
        let mut input = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            input.push(i as f32 / frames as f32);
            input.push(1.0);
        }
        let out = resample_interleaved(input, 2, 44_100, 88_200);
        for frame in out.chunks_exact(2) {
            assert!(
                (frame[1] - 1.0).abs() < 1e-6,
                "right channel drifted: {}",
                frame[1]
            );
        }
        // And the left channel should still be monotonically rising.
        let lefts: Vec<f32> = out.chunks_exact(2).map(|f| f[0]).collect();
        assert!(lefts.windows(2).all(|w| w[1] >= w[0] - 1e-6));
    }

    #[test]
    fn a_sine_keeps_its_frequency_in_wall_clock_time() {
        // The actual bug this module exists to prevent: 1 second of 440Hz
        // must still be 1 second of 440Hz after conversion, not 2.177
        // seconds' worth crammed into one.
        let from = 44_100u32;
        let to = 96_000u32;
        let input: Vec<f32> = (0..from)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / from as f32).sin())
            .collect();
        let out = resample_interleaved(input, 1, from, to);

        assert_eq!(out.len(), to as usize, "one second in, one second out");

        // Count zero crossings: 440Hz for 1s crosses zero ~880 times
        // regardless of sample rate.
        let crossings = out.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        assert!(
            (439..=441).contains(&crossings),
            "expected ~440 positive-going crossings, got {crossings}"
        );
    }
}
