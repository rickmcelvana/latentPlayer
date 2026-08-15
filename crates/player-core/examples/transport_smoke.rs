//! Live check of the native transport (Phase 1 M1): position, seek, volume,
//! pause. Synthesises its own tone, so it needs no file and no arguments:
//!
//!   cargo run -p player-core --example transport_smoke
//!
//! Opens the real default output device, so it is a genuine end-to-end test
//! of `Playback` rather than a unit test of arithmetic -- including the
//! resample-to-device-rate path, since the tone is generated at 44.1kHz and
//! most devices don't run there. Deliberately quiet (gain 0.1): it makes
//! real sound on the real default device.

use std::thread::sleep;
use std::time::Duration;

use player_core::{DecodedAudio, Playback};

const SRC_RATE: u32 = 44_100;
const SECONDS: u32 = 30;

fn check(label: &str, ok: bool, detail: String) {
    println!("  [{}] {label}: {detail}", if ok { "PASS" } else { "FAIL" });
}

/// True when `got` is within `tol` of `want`.
fn near(got: f64, want: f64, tol: f64) -> bool {
    (got - want).abs() <= tol
}

fn main() {
    let frames = (SRC_RATE * SECONDS) as usize;
    let mut samples = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let t = 2.0 * std::f32::consts::PI * 220.0 * i as f32 / SRC_RATE as f32;
        samples.push(t.sin() * 0.7);
        samples.push(t.cos() * 0.7);
    }
    let audio = DecodedAudio {
        samples,
        channels: 2,
        sample_rate: SRC_RATE,
    };
    println!(
        "source: {:.1}s @ {SRC_RATE} Hz stereo",
        audio.duration_seconds()
    );

    let playback = Playback::open(audio).expect("failed to open output device");
    playback.set_volume(0.1); // be kind to whoever is at the keyboard
    println!(
        "device:  {} ch @ {} Hz (resampled from {SRC_RATE})",
        playback.output_channels(),
        playback.output_sample_rate()
    );

    check(
        "duration survives resampling",
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
        near(after_seek, 20.5, 0.15),
        format!("{after_seek:.3}s after 0.5s more"),
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

    playback.seek(1e9);
    check(
        "seeking past the end parks at the end",
        playback.is_finished() && near(playback.position_seconds(), SECONDS as f64, 0.05),
        format!(
            "position {:.3}s, finished={}",
            playback.position_seconds(),
            playback.is_finished()
        ),
    );
}
