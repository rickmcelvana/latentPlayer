//! Live check of the queue and the gapless hand-off (Phase 1 M3). Synthesises
//! its own WAV files, so it needs no fixture and no arguments:
//!
//!   cargo run -p player-core --example gapless_smoke
//!
//! Opens the real default output device and makes real (quiet) sound, because
//! the thing being tested is whether audio crosses a track boundary without a
//! hole in it -- which is a property of the actual output path, not of
//! arithmetic.
//!
//! The two tracks are told apart by ear-equivalent measurement rather than by
//! trusting the engine's own reports:
//!
//!   left channel   a tone, 220Hz in the first track and 440Hz in the second.
//!                  Counting zero crossings in the scope says which track the
//!                  device is actually playing, independently of any counter
//!                  the engine publishes.
//!   right channel  a ramp from 0 to 1 across the track, so any sample says
//!                  where in its track it came from.
//!
//! The hard checks are the two that a plausible-looking hand-off still gets
//! wrong: whether the seam contains silence (the decoder opening the next file
//! while the ring runs dry), and whether a seek arriving while the decoder has
//! already moved on scrubs the track being heard or the one being decoded.

use std::io::Write;
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

use player_core::{LocalFile, Playback};

const SRC_RATE: u32 = 44_100;

const FIRST_SECONDS: u32 = 3;
const FIRST_HZ: f32 = 220.0;
/// Deliberately a different length from the first, so "the duration changed at
/// the hand-off" is a real observation rather than a coincidence.
const SECOND_SECONDS: u32 = 5;
const SECOND_HZ: f32 = 440.0;

/// Frames of scope history to measure at a time. ~85ms at 48kHz, comfortably
/// inside the ring's ~170ms, so successive polls overlap and nothing played is
/// missed.
const WINDOW_FRAMES: usize = 4096;

/// Consecutive zero samples in the tone channel that count as a dropout. A
/// 220Hz sine crosses zero every ~109 samples but is only *exactly* zero for a
/// sample or so at a time; a real gap at the seam would be hundreds of samples
/// of true silence, so this sits well clear of both.
const DROPOUT_SAMPLES: usize = 32;

fn check(label: &str, ok: bool, detail: String) {
    println!("  [{}] {label}: {detail}", if ok { "PASS" } else { "FAIL" });
}

fn near(got: f64, want: f64, tol: f64) -> bool {
    (got - want).abs() <= tol
}

/// Writes a 16-bit stereo WAV: left is a `hz` tone, right ramps 0 -> 1 across
/// the whole track.
fn write_wav(path: &Path, seconds: u32, hz: f32) -> std::io::Result<()> {
    let frames = (SRC_RATE * seconds) as usize;
    let (channels, bits) = (2u16, 16u16);
    let block_align = channels * bits / 8;
    let data_len = frames * block_align as usize;

    let file = std::fs::File::create(path)?;
    let mut out = std::io::BufWriter::new(file);
    out.write_all(b"RIFF")?;
    out.write_all(&((36 + data_len) as u32).to_le_bytes())?;
    out.write_all(b"WAVEfmt ")?;
    out.write_all(&16u32.to_le_bytes())?;
    out.write_all(&1u16.to_le_bytes())?; // PCM
    out.write_all(&channels.to_le_bytes())?;
    out.write_all(&SRC_RATE.to_le_bytes())?;
    out.write_all(&(SRC_RATE * block_align as u32).to_le_bytes())?;
    out.write_all(&block_align.to_le_bytes())?;
    out.write_all(&bits.to_le_bytes())?;
    out.write_all(b"data")?;
    out.write_all(&(data_len as u32).to_le_bytes())?;
    for i in 0..frames {
        let t = 2.0 * std::f32::consts::PI * hz * i as f32 / SRC_RATE as f32;
        let left = (t.sin() * 0.7 * 32_767.0) as i16;
        let right = ((i as f32 / frames as f32) * 32_767.0) as i16;
        out.write_all(&left.to_le_bytes())?;
        out.write_all(&right.to_le_bytes())?;
    }
    out.flush()
}

/// One channel lifted out of a scope window, which is interleaved in the
/// device's layout (M3 moved channel adaptation ahead of the ring).
fn channel(playback: &Playback, index: usize) -> Vec<f32> {
    let stride = playback.output_channels().max(1) as usize;
    let window = playback.scope_window(WINDOW_FRAMES * stride);
    window
        .iter()
        .skip(index.min(stride - 1))
        .step_by(stride)
        .copied()
        .collect()
}

/// Frequency of the tone currently reaching the device, from its zero-crossing
/// rate. Says which track is really playing without asking the engine.
fn played_hz(playback: &Playback) -> f64 {
    let tone = channel(playback, 0);
    if tone.len() < 2 {
        return f64::NAN;
    }
    let crossings = tone
        .windows(2)
        .filter(|pair| (pair[0] < 0.0) != (pair[1] < 0.0))
        .count();
    let seconds = tone.len() as f64 / playback.output_sample_rate() as f64;
    crossings as f64 / seconds / 2.0
}

/// Where in its own track the audio reaching the device came from: 0.0 at the
/// start, 1.0 at the end.
fn played_fraction(playback: &Playback) -> f64 {
    let ramp = channel(playback, 1);
    if ramp.is_empty() {
        return f64::NAN;
    }
    ramp.iter().map(|&s| s as f64).sum::<f64>() / ramp.len() as f64
}

/// Longest run of exactly-zero samples in the tone channel -- the fingerprint
/// of the ring running dry.
fn longest_silence(playback: &Playback) -> usize {
    let mut longest = 0;
    let mut run = 0;
    for sample in channel(playback, 0) {
        if sample == 0.0 {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    longest
}

/// What the seam looked like, gathered while the music crossed it.
struct Seam {
    handed_off: bool,
    longest_silence: usize,
    reported_finished: bool,
}

/// Watches until the hand-off lands, sampling fast enough that no played audio
/// escapes the overlapping scope windows.
fn watch_seam(playback: &Playback, timeout: Duration) -> Seam {
    let start_index = playback.track_index();
    let deadline = Instant::now() + timeout;
    let mut seam = Seam {
        handed_off: false,
        longest_silence: 0,
        reported_finished: false,
    };

    while Instant::now() < deadline {
        seam.longest_silence = seam.longest_silence.max(longest_silence(playback));
        // A hand-off is not an ending, so `is_finished` must stay false right
        // across it -- a caller watching that flag to advance a playlist would
        // otherwise double-advance at every track.
        if playback.is_finished() {
            seam.reported_finished = true;
        }
        if playback.track_index() > start_index {
            seam.handed_off = true;
            // Keep sampling briefly: a dropout at the seam lands just after
            // the boundary as often as just before it.
            sleep(Duration::from_millis(120));
            seam.longest_silence = seam.longest_silence.max(longest_silence(playback));
            break;
        }
        sleep(Duration::from_millis(25));
    }
    seam
}

fn main() {
    let dir = std::env::temp_dir();
    let first = dir.join("latentplayer_gapless_first.wav");
    let second = dir.join("latentplayer_gapless_second.wav");
    write_wav(&first, FIRST_SECONDS, FIRST_HZ).expect("failed to write the first test WAV");
    write_wav(&second, SECOND_SECONDS, SECOND_HZ).expect("failed to write the second test WAV");

    let playback = Playback::open(Box::new(LocalFile::new(&first))).expect("failed to open output");
    playback.set_volume(0.1); // be kind to whoever is at the keyboard
    println!(
        "device:  {} ch @ {} Hz (resampled from {SRC_RATE})",
        playback.output_channels(),
        playback.output_sample_rate()
    );
    if playback.output_channels() < 2 {
        println!(
            "note:    this device is mono, so the ramp channel never reaches the scope\n\
             \x20        and the position checks below are meaningless. Pick a stereo\n\
             \x20        output device and rerun."
        );
    }

    check(
        "the queue starts on the first track",
        near(playback.duration_seconds(), FIRST_SECONDS as f64, 0.05)
            && playback.track_index() == 0,
        format!(
            "{:.3}s, track_index {}",
            playback.duration_seconds(),
            playback.track_index()
        ),
    );

    playback.set_next(Box::new(LocalFile::new(&second)));
    playback.play();
    sleep(Duration::from_millis(1500));

    check(
        "queuing a track does not disturb the one playing",
        near(played_hz(&playback), FIRST_HZ as f64, 15.0) && playback.track_index() == 0,
        format!(
            "{:.0}Hz reaching the device (want ~{FIRST_HZ:.0}Hz), track_index {}",
            played_hz(&playback),
            playback.track_index()
        ),
    );

    // --- the seek-during-hand-off window ---------------------------------
    //
    // The decoder runs up to a ring (~2s) ahead, so 1.5s into a 3s track it
    // has already finished this track and moved on to the queued one. A seek
    // arriving now must scrub the track being *heard*, not the one being
    // decoded. Getting this wrong jumps into the next song, and only in the
    // last couple of seconds of every track -- which is exactly the kind of
    // bug that survives casual testing.
    playback.seek(0.5);
    sleep(Duration::from_millis(400));
    let fraction = played_fraction(&playback);
    check(
        "seeking near the end of a track stays in that track",
        near(played_hz(&playback), FIRST_HZ as f64, 15.0)
            && playback.track_index() == 0
            && near(fraction, 0.9 / FIRST_SECONDS as f64, 0.06),
        format!(
            "{:.0}Hz, track_index {}, ramp reads {fraction:.3} (want ~{:.3}; ~0.0 with a \
             440Hz tone would mean it scrubbed the queued track instead)",
            played_hz(&playback),
            playback.track_index(),
            0.9 / FIRST_SECONDS as f64
        ),
    );

    // --- the seam --------------------------------------------------------
    let seam = watch_seam(&playback, Duration::from_secs(6));
    check(
        "the hand-off happens when the first track runs out",
        seam.handed_off,
        format!("track_index {}", playback.track_index()),
    );
    check(
        "no silence at the seam",
        seam.longest_silence < DROPOUT_SAMPLES,
        format!(
            "longest run of silence across the boundary: {} samples (a dropout would be \
             hundreds)",
            seam.longest_silence
        ),
    );
    check(
        "a hand-off is not an ending",
        !seam.reported_finished,
        format!("is_finished stayed {}", seam.reported_finished),
    );

    check(
        "the new track's length is published when its audio arrives",
        near(playback.duration_seconds(), SECOND_SECONDS as f64, 0.05),
        format!(
            "{:.3}s (want ~{SECOND_SECONDS}s)",
            playback.duration_seconds()
        ),
    );
    check(
        "the position restarts in the new track",
        playback.position_seconds() < 0.75,
        format!("{:.3}s into the new track", playback.position_seconds()),
    );
    check(
        "the audio really is the second track",
        near(played_hz(&playback), SECOND_HZ as f64, 20.0),
        format!(
            "{:.0}Hz reaching the device (want ~{SECOND_HZ:.0}Hz)",
            played_hz(&playback)
        ),
    );

    // --- the end of the queue --------------------------------------------
    //
    // Same event as the seam above, minus anything queued behind it. That is
    // the only difference between "next track" and "stop".
    playback.seek(SECOND_SECONDS as f64 - 0.4);
    sleep(Duration::from_millis(1200));
    check(
        "running out of queue is an ending",
        playback.is_finished(),
        format!(
            "finished={} at {:.3}s",
            playback.is_finished(),
            playback.position_seconds()
        ),
    );

    // --- queuing after the music stopped ---------------------------------
    //
    // The decoder parks when it runs out; a track queued while it is parked
    // has to wake it rather than wait for a seek.
    playback.set_next(Box::new(LocalFile::new(&first)));
    sleep(Duration::from_millis(600));
    check(
        "a track queued after the end starts playing",
        !playback.is_finished() && near(played_hz(&playback), FIRST_HZ as f64, 15.0),
        format!(
            "finished={}, {:.0}Hz reaching the device (want ~{FIRST_HZ:.0}Hz)",
            playback.is_finished(),
            played_hz(&playback)
        ),
    );

    drop(playback);
    let _ = std::fs::remove_file(&first);
    let _ = std::fs::remove_file(&second);
}
