//! Live check of the native engine -- transport (Phase 1 M1) and streaming
//! decode (M2). Synthesises its own WAV files, so it needs no fixture and no
//! arguments:
//!
//!   cargo run -p player-core --example transport_smoke
//!
//! Opens the real default output device, so it is a genuine end-to-end test of
//! `Playback` rather than a unit test of arithmetic -- including the
//! resample-to-device-rate path, since the tone is generated at 44.1kHz and
//! most devices don't run there. Deliberately quiet (gain 0.1): it makes real
//! sound on the real default device.
//!
//! The synthesised track encodes its own timeline: the left channel is a tone,
//! the right is a ramp from 0 to 1 over the track. That makes "is the audio
//! coming from where the transport says it is" a measurable question rather
//! than something only a listener could answer -- which is the check that
//! catches a seek leaving stale audio in the ring.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use player_core::{LocalFile, Playback};

const SRC_RATE: u32 = 44_100;
const SECONDS: u32 = 30;
/// A track long enough that decoding it whole would be obvious in the clock.
const LONG_SECONDS: u32 = 300;

fn check(label: &str, ok: bool, detail: String) {
    println!("  [{}] {label}: {detail}", if ok { "PASS" } else { "FAIL" });
}

/// True when `got` is within `tol` of `want`.
fn near(got: f64, want: f64, tol: f64) -> bool {
    (got - want).abs() <= tol
}

/// Writes a 16-bit stereo WAV: left is a 220Hz tone, right ramps 0 -> 1 across
/// the whole track so any sample says where in the track it came from.
fn write_wav(path: &Path, seconds: u32) -> std::io::Result<()> {
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
        let t = 2.0 * std::f32::consts::PI * 220.0 * i as f32 / SRC_RATE as f32;
        let left = (t.sin() * 0.7 * 32_767.0) as i16;
        let right = ((i as f32 / frames as f32) * 32_767.0) as i16;
        out.write_all(&left.to_le_bytes())?;
        out.write_all(&right.to_le_bytes())?;
    }
    out.flush()
}

/// Where in the track the audio currently reaching the speakers came from,
/// read off the ramp in the right channel: 0.0 at the start, 1.0 at the end.
///
/// The window is interleaved in the *output* layout (M3 moved channel
/// adaptation ahead of the ring), so the stride is the device's channel count,
/// not the file's two. Channel 1 carries the ramp on any device with a second
/// channel; a mono device gets the tone instead, which averages to nothing --
/// `main` says so rather than letting the checks fail mysteriously.
fn played_position_fraction(playback: &Playback) -> f64 {
    let stride = playback.output_channels().max(1) as usize;
    let window = playback.scope_window(4096 * stride);
    let ramp: Vec<f32> = window
        .iter()
        .skip(1.min(stride - 1))
        .step_by(stride)
        .copied()
        .collect();
    if ramp.is_empty() {
        return f64::NAN;
    }
    ramp.iter().map(|&s| s as f64).sum::<f64>() / ramp.len() as f64
}

fn main() {
    let dir = std::env::temp_dir();
    let short = dir.join("latentplayer_transport_smoke.wav");
    let long = dir.join("latentplayer_transport_smoke_long.wav");
    write_wav(&short, SECONDS).expect("failed to write the test WAV");
    write_wav(&long, LONG_SECONDS).expect("failed to write the long test WAV");

    // --- M2: opening must not scale with track length ---------------------
    //
    // Whole-file decode made `open` proportional to the track: ten times the
    // audio, ten times the wait and ten times the memory. Streaming should
    // make the two effectively the same, since both only decode as far as the
    // prebuffer.
    let short_open = time_open(&short);
    let long_open = time_open(&long);
    let ratio = long_open.as_secs_f64() / short_open.as_secs_f64().max(1e-6);
    check(
        "open() does not scale with track length",
        ratio < 3.0,
        format!(
            "{SECONDS}s took {:.0}ms, {LONG_SECONDS}s ({}x the audio) took {:.0}ms -- {ratio:.1}x",
            short_open.as_secs_f64() * 1000.0,
            LONG_SECONDS / SECONDS,
            long_open.as_secs_f64() * 1000.0,
        ),
    );

    let playback =
        Playback::open(Box::new(LocalFile::new(&short))).expect("failed to open output device");
    playback.set_volume(0.1); // be kind to whoever is at the keyboard
    println!(
        "device:  {} ch @ {} Hz (resampled from {SRC_RATE})",
        playback.output_channels(),
        playback.output_sample_rate()
    );
    if playback.output_channels() < 2 {
        println!(
            "note:    this device is mono, so the right-channel ramp never reaches the\n\
             \x20        scope and every position check below is meaningless. Pick a stereo\n\
             \x20        output device and rerun."
        );
    }

    check(
        "duration comes from the container",
        near(playback.duration_seconds(), SECONDS as f64, 0.05),
        format!("{:.3}s (want ~{SECONDS}s)", playback.duration_seconds()),
    );

    playback.play();
    sleep(Duration::from_millis(1000));
    let after_1s = playback.position_seconds();
    check(
        "position advances in real time",
        near(after_1s, 1.0, 0.15),
        format!("{after_1s:.3}s after 1.0s of wall clock"),
    );

    check(
        "audio reaching the device matches the reported position",
        near(
            played_position_fraction(&playback),
            1.0 / SECONDS as f64,
            0.03,
        ),
        format!(
            "ramp reads {:.3} (want ~{:.3})",
            played_position_fraction(&playback),
            1.0 / SECONDS as f64
        ),
    );

    playback.seek(20.0);
    let at_seek = playback.position_seconds();
    check(
        "seek lands where asked",
        near(at_seek, 20.0, 0.01),
        format!("{at_seek:.3}s (want 20.000s)"),
    );

    sleep(Duration::from_millis(500));
    let after_seek = playback.position_seconds();
    check(
        "playback continues from the seek point",
        near(after_seek, 20.5, 0.2),
        format!("{after_seek:.3}s after 0.5s more"),
    );

    // The one M2 can get wrong invisibly: a seek that leaves the audio already
    // queued in the ring to play out would sound like up to two seconds of the
    // old position before the new one arrives. The ramp says which it is.
    let fraction = played_position_fraction(&playback);
    check(
        "no pre-seek audio survives the seek",
        near(fraction, 20.5 / SECONDS as f64, 0.05),
        format!(
            "ramp reads {fraction:.3} (want ~{:.3}; ~0.03 would mean the pre-seek audio was still playing)",
            20.5 / SECONDS as f64
        ),
    );

    playback.pause();
    let paused_at = playback.position_seconds();
    sleep(Duration::from_millis(300));
    let still = playback.position_seconds();
    check(
        "pause freezes the position",
        near(still, paused_at, 0.01),
        format!("{paused_at:.3}s -> {still:.3}s"),
    );

    check(
        "a paused track is not a finished one",
        !playback.is_finished(),
        format!("finished={}", playback.is_finished()),
    );

    playback.set_volume(0.25);
    check(
        "volume round-trips",
        near(playback.volume() as f64, 0.25, 1e-6),
        format!("{:.3}", playback.volume()),
    );
    playback.set_volume(5.0);
    check(
        "volume clamps to 1.0",
        near(playback.volume() as f64, 1.0, 1e-6),
        format!("{:.3} after set_volume(5.0)", playback.volume()),
    );
    playback.set_volume(0.1);

    // --- end of track -----------------------------------------------------
    playback.seek(SECONDS as f64 - 0.3);
    playback.play();
    let mut finished = false;
    for _ in 0..40 {
        sleep(Duration::from_millis(50));
        if playback.is_finished() {
            finished = true;
            break;
        }
    }
    check(
        "playing to the end reports finished",
        finished && near(playback.position_seconds(), SECONDS as f64, 0.3),
        format!(
            "position {:.3}s, finished={}",
            playback.position_seconds(),
            playback.is_finished()
        ),
    );

    playback.seek(5.0);
    sleep(Duration::from_millis(200));
    check(
        "seeking back un-finishes the track",
        !playback.is_finished() && near(playback.position_seconds(), 5.2, 0.25),
        format!(
            "position {:.3}s, finished={}",
            playback.position_seconds(),
            playback.is_finished()
        ),
    );

    drop(playback);
    let _ = std::fs::remove_file(&short);
    let _ = std::fs::remove_file(&long);
}

/// How long `Playback::open` takes, including its prebuffer wait.
fn time_open(path: &PathBuf) -> Duration {
    let start = Instant::now();
    let playback =
        Playback::open(Box::new(LocalFile::new(path))).expect("failed to open output device");
    let elapsed = start.elapsed();
    drop(playback);
    elapsed
}
