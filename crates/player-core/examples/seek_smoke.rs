//! Does seeking give you the *right* audio? Run with:
//!   cargo run -p player-core --example seek_smoke -- <path>
//!
//! Decodes only -- no output device, no listening required. For each of a few
//! positions it decodes the same moment of the track two ways and compares
//! them:
//!
//!   reference: open at the start and decode straight through to the target,
//!              which is the audio a listener hears when playing normally
//!   seeked:    open, seek to the target, decode
//!
//! If seeking is right these agree. If the decoder's pre-roll handling is
//! wrong they don't, and the specific way they disagree says how.
//!
//! Written for the MP3 question this exists to answer (2026-08-15). Symphonia's
//! MP3 demuxer deliberately seeks *before* the requested time, back to the
//! frame holding the first byte of the target frame's main_data, because MP3
//! frames borrow data from their predecessors through the bit reservoir. It
//! reports that earlier position as `actual_ts` and expects the caller to
//! decode the difference and throw it away -- which is what
//! `StreamingDecoder::seek`'s `discard_frames` does. Those discarded pre-roll
//! frames are where symphonia logs
//!
//!   [symphonia_bundle_mp3::layer3][WARN] mpa: invalid main_data_begin,
//!   underflow by N bytes
//!
//! and zeroes the affected granules. The question this example answers is
//! whether any of that zeroing survives into the audio that actually plays:
//! the `leading silence` figure below would be a whole frame or more if it
//! did, and the two decodes would diverge.

use player_core::{LocalFile, StreamingDecoder};

/// Frames to compare at each position -- a few hundred milliseconds, enough to
/// cover any pre-roll that leaked through.
const COMPARE_FRAMES: usize = 16_384;

/// Fractions of the track to test. Avoids 0.0 (nothing to seek past) and the
/// very end (nothing left to compare).
const POSITIONS: [f64; 3] = [0.25, 0.5, 0.75];

fn check(label: &str, ok: bool, detail: String) {
    println!("  [{}] {label}: {detail}", if ok { "PASS" } else { "FAIL" });
}

/// Collects `frames` frames starting at the decoder's current position.
fn collect(decoder: &mut StreamingDecoder, frames: usize) -> Vec<f32> {
    let channels = decoder.channels().max(1) as usize;
    let wanted = frames * channels;
    let mut out = Vec::with_capacity(wanted);
    while out.len() < wanted {
        match decoder.next_block() {
            Ok(Some(block)) => out.extend_from_slice(block),
            Ok(None) => break,
            Err(e) => {
                eprintln!("decode error: {e}");
                break;
            }
        }
    }
    out.truncate(wanted);
    out
}

/// Decodes from the start, discarding everything before `seconds`. This is the
/// audio a listener hears when the track simply plays through.
fn decode_through(path: &str, seconds: f64, frames: usize) -> Vec<f32> {
    let mut decoder = StreamingDecoder::open(&LocalFile::new(path)).expect("open failed");
    let channels = decoder.channels().max(1) as usize;
    let rate = decoder.sample_rate() as f64;
    let mut skip = (seconds * rate) as usize * channels;

    while skip > 0 {
        match decoder.next_block() {
            Ok(Some(block)) => {
                if block.len() <= skip {
                    skip -= block.len();
                } else {
                    // Landed mid-block: keep the tail and top up from there.
                    let mut out = block[skip..].to_vec();
                    out.extend_from_slice(&collect(&mut decoder, frames));
                    out.truncate(frames * channels);
                    return out;
                }
            }
            Ok(None) => return Vec::new(),
            Err(e) => {
                eprintln!("decode error: {e}");
                return Vec::new();
            }
        }
    }
    collect(&mut decoder, frames)
}

/// How many leading samples are exactly zero. A zeroed granule is what
/// symphonia substitutes for main data it doesn't have, so a run of these at a
/// seek point is the fingerprint of pre-roll leaking into playback.
fn leading_zeros(samples: &[f32]) -> usize {
    samples.iter().take_while(|s| **s == 0.0).count()
}

/// Root-mean-square difference between two decodes of the same audio, relative
/// to the audio's own level. Relative because "how far apart" only means
/// something against how loud the passage is.
fn relative_difference(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return f64::NAN;
    }
    let mut diff_sq = 0.0;
    let mut ref_sq = 0.0;
    for i in 0..n {
        let d = (a[i] - b[i]) as f64;
        diff_sq += d * d;
        ref_sq += (a[i] as f64) * (a[i] as f64);
    }
    if ref_sq <= 0.0 {
        return 0.0;
    }
    (diff_sq / ref_sq).sqrt()
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: seek_smoke <path>");

    let probe = StreamingDecoder::open(&LocalFile::new(&path)).expect("open failed");
    let channels = probe.channels();
    let rate = probe.sample_rate();
    let Some(duration) = probe.duration_seconds() else {
        eprintln!("this container states no duration; seek_smoke needs one to pick positions");
        return;
    };
    drop(probe);
    println!("{path}: {duration:.2}s, {channels} ch @ {rate} Hz");
    println!(
        "comparing {:.0}ms at each position\n",
        COMPARE_FRAMES as f64 / rate as f64 * 1000.0
    );

    for fraction in POSITIONS {
        let target = duration * fraction;
        println!("-- {:.1}% ({target:.2}s)", fraction * 100.0);

        let reference = decode_through(&path, target, COMPARE_FRAMES);

        let mut decoder = StreamingDecoder::open(&LocalFile::new(&path)).expect("open failed");
        let landed = decoder.seek(target).expect("seek failed");
        let seeked = collect(&mut decoder, COMPARE_FRAMES);

        if reference.is_empty() || seeked.is_empty() {
            check(
                "decoded audio at this position",
                false,
                "got nothing".into(),
            );
            continue;
        }

        check(
            "seek reports the requested position",
            (landed - target).abs() < 0.01,
            format!("{landed:.3}s (asked for {target:.3}s)"),
        );

        // The pre-roll test. One frame of MP3 is 1152 frames of audio; anything
        // approaching that means zeroed granules reached the output.
        let zeros = leading_zeros(&seeked) / channels.max(1) as usize;
        check(
            "no silent pre-roll survives into playback",
            zeros < 64,
            format!("{zeros} silent frames at the seek point"),
        );

        // The real question: is this the same audio you'd have heard playing
        // through? Not bit-exact for a lossy codec -- the reservoir state
        // differs slightly either side of a seek -- but it should be close.
        let difference = relative_difference(&reference, &seeked);
        check(
            "seeked audio matches playing through to the same point",
            difference < 0.05,
            format!("{:.2}% RMS difference", difference * 100.0),
        );
        println!();
    }
}
