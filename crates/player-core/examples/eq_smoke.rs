//! Live check of the ten-band EQ (Phase 1 M8). Synthesises its own WAV, so it
//! needs no fixture and no arguments:
//!
//!   cargo run -p player-core --example eq_smoke
//!
//! The native counterpart of `/eq.html` in the web build, running the same
//! checks against the same claims so the two shells can be held to one
//! standard.
//!
//! It opens the real default output device and makes real (quiet) sound,
//! because what is being tested is the whole path -- ring, callback, filters,
//! tap, analysis thread -- not the biquads, which `eq.rs`'s own tests already
//! pin down against analytic answers. The measuring instrument is the spectrum
//! analyser, which is the point twice over: every check has an arithmetic
//! answer (a +12dB slider must move its own band by 12dB and its neighbours by
//! none), and using the meters to measure the EQ is only possible *because* the
//! EQ runs before the analysis tap (M8 decision 2). A build with those two the
//! other way round fails every check below.

use std::io::Write;
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

use player_core::eq::{magnitude_response, response_frequencies, EqSettings, BAND_FREQUENCIES};
use player_core::{LocalFile, Playback};

const SRC_RATE: u32 = 44_100;
const TONE_SECONDS: u32 = 30;

/// The two tones the test WAV carries. One sits in the 1kHz band and one in the
/// 8kHz band -- five octaves apart, so a boost of either has nowhere to hide.
const LOW_HZ: f32 = 1000.0;
const HIGH_HZ: f32 = 8000.0;

/// Amplitude of each tone: -12dBFS, so a +12dB boost still has somewhere to go
/// before it clips.
const TONE_AMPLITUDE: f32 = 0.25;

/// Which slider each tone belongs to.
const LOW_BAND: usize = 5;
const HIGH_BAND: usize = 8;

/// How long to let the display catch up after a change. A rise is immediate,
/// but a fall decays at 30dB/s, so a 12dB cut takes 0.4s to arrive.
const SETTLE: Duration = Duration::from_millis(900);

fn check(label: &str, ok: bool, detail: String) {
    println!("  [{}] {label}: {detail}", if ok { "PASS" } else { "FAIL" });
}

/// A 16-bit stereo WAV of two summed sines. Same signal the web harness makes.
fn write_wav(path: &Path) -> std::io::Result<()> {
    let frames = (SRC_RATE * TONE_SECONDS) as usize;
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
        let t = i as f32 / SRC_RATE as f32;
        let low = (2.0 * std::f32::consts::PI * LOW_HZ * t).sin();
        let high = (2.0 * std::f32::consts::PI * HIGH_HZ * t).sin();
        let sample = ((low + high) * TONE_AMPLITUDE * 32_767.0) as i16;
        out.write_all(&sample.to_le_bytes())?;
        out.write_all(&sample.to_le_bytes())?;
    }
    out.flush()
}

/// Index of the display band whose centre is nearest `hz`, on the log scale the
/// bands are spaced on.
fn band_at(bands: &[f32], hz: f32) -> usize {
    bands
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            (a.log10() - hz.log10())
                .abs()
                .total_cmp(&(b.log10() - hz.log10()).abs())
        })
        .map(|(i, _)| i)
        .unwrap_or(0)
}

fn settings_with(band: usize, gain_db: f32) -> EqSettings {
    let mut settings = EqSettings::flat();
    settings.gains_db[band] = gain_db;
    settings
}

/// What the drawn curve claims at `hz`.
fn curve_at(settings: &EqSettings, rate: u32, hz: f32) -> f32 {
    let frequencies = response_frequencies();
    let curve = magnitude_response(settings, rate);
    frequencies
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| (*a - hz).abs().total_cmp(&(*b - hz).abs()))
        .map(|(i, _)| curve[i])
        .unwrap_or(0.0)
}

fn main() {
    let path = std::env::temp_dir().join("latentplayer_eq.wav");
    write_wav(&path).expect("write test tone");

    let playback = Playback::open(Box::new(LocalFile::new(&path))).expect("open test tone");
    playback.set_volume(0.1); // be kind to whoever is at the keyboard
    let rate = playback.output_sample_rate();
    let bands = playback.spectrum_band_centres().to_vec();
    let low = band_at(&bands, LOW_HZ);
    let high = band_at(&bands, HIGH_HZ);
    println!(
        "output:  {rate} Hz, tones at {:.0}Hz (display band {low}) and {:.0}Hz (band {high})",
        LOW_HZ, HIGH_HZ
    );

    check(
        "the EQ starts flat and enabled",
        playback.eq() == EqSettings::flat()
            && playback.eq_response().iter().all(|db| db.abs() < 0.001),
        format!(
            "{} sliders at 0, preamp 0, curve within {:.4}dB of flat",
            player_core::EQ_BANDS,
            playback
                .eq_response()
                .iter()
                .fold(0.0f32, |m, db| m.max(db.abs()))
        ),
    );

    playback.play();
    sleep(SETTLE);
    let flat = playback.spectrum();
    let flat_loudness = playback.meters().momentary_lufs;
    println!(
        "baseline: {:.2}dBFS at {:.0}Hz, {:.2}dBFS at {:.0}Hz, {:.2} LUFS",
        flat[low], bands[low], flat[high], bands[high], flat_loudness
    );

    // --- one slider, one band ---

    let boost = settings_with(LOW_BAND, 12.0);
    playback.set_eq(boost);
    sleep(SETTLE);
    let boosted = playback.spectrum();
    let moved = boosted[low] - flat[low];
    check(
        "a +12dB slider moves its own band by 12dB",
        (moved - 12.0).abs() < 1.5,
        format!(
            "{:.2}dBFS -> {:.2}dBFS ({moved:+.2}dB) at {:.0}Hz",
            flat[low], boosted[low], bands[low]
        ),
    );
    let bystander = boosted[high] - flat[high];
    check(
        "it leaves the other band alone",
        bystander.abs() < 1.5,
        format!(
            "{:.2}dBFS -> {:.2}dBFS ({bystander:+.2}dB) at {:.0}Hz",
            flat[high], boosted[high], bands[high]
        ),
    );
    check(
        "the audio matches the curve the UI draws",
        (moved - curve_at(&boost, rate, LOW_HZ)).abs() < 1.5,
        format!(
            "audio moved {moved:+.2}dB, curve says {:+.2}dB at {:.0}Hz",
            curve_at(&boost, rate, LOW_HZ),
            LOW_HZ
        ),
    );

    let cut = settings_with(HIGH_BAND, -12.0);
    playback.set_eq(cut);
    sleep(SETTLE);
    let after_cut = playback.spectrum();
    check(
        "a -12dB slider cuts its own band by 12dB",
        ((after_cut[high] - flat[high]) + 12.0).abs() < 1.5,
        format!(
            "{:.2}dBFS -> {:.2}dBFS ({:+.2}dB) at {:.0}Hz",
            flat[high],
            after_cut[high],
            after_cut[high] - flat[high],
            bands[high]
        ),
    );
    check(
        "and releases the band the previous setting had boosted",
        (after_cut[low] - flat[low]).abs() < 1.5,
        format!(
            "{:.2}dBFS against a flat {:.2}dBFS",
            after_cut[low], flat[low]
        ),
    );

    // --- the preamp ---

    let mut preamp = EqSettings::flat();
    preamp.preamp_db = -6.0;
    playback.set_eq(preamp);
    sleep(SETTLE);
    let quieter = playback.spectrum();
    check(
        "the preamp moves the whole spectrum together",
        ((quieter[low] - flat[low]) + 6.0).abs() < 1.0
            && ((quieter[high] - flat[high]) + 6.0).abs() < 1.0,
        format!(
            "{:+.2}dB at {:.0}Hz and {:+.2}dB at {:.0}Hz for a -6dB preamp",
            quieter[low] - flat[low],
            bands[low],
            quieter[high] - flat[high],
            bands[high]
        ),
    );

    // The whole reason the filters sit ahead of the tap: what the loudness
    // meter reports is what the listener is actually being played (M8
    // decision 2). A -6dB preamp is broadband, so it moves LUFS by 6.
    let quieter_loudness = playback.meters().momentary_lufs;
    check(
        "the loudness meter measures the EQ",
        ((quieter_loudness - flat_loudness) + 6.0).abs() < 1.0,
        format!("{flat_loudness:.2} LUFS -> {quieter_loudness:.2} LUFS"),
    );

    // ...while the volume control still does not, which is the other half of
    // the same decision: monitoring level is not part of the programme.
    playback.set_volume(0.5);
    sleep(SETTLE);
    let louder = playback.spectrum();
    check(
        "the volume control still does not move the meters",
        (louder[low] - quieter[low]).abs() < 1.5,
        format!(
            "{:.2}dBFS at volume 0.1, {:.2}dBFS at 0.5",
            quieter[low], louder[low]
        ),
    );
    playback.set_volume(0.1);

    // --- bypass ---

    let mut bypassed = settings_with(LOW_BAND, 12.0);
    bypassed.preamp_db = -6.0;
    bypassed.enabled = false;
    playback.set_eq(bypassed);
    sleep(SETTLE);
    let off = playback.spectrum();
    check(
        "bypass returns the audio to where it started",
        (off[low] - flat[low]).abs() < 1.0 && (off[high] - flat[high]).abs() < 1.0,
        format!(
            "{:.2}dBFS and {:.2}dBFS against a flat {:.2} and {:.2}",
            off[low], off[high], flat[low], flat[high]
        ),
    );
    check(
        "and flattens the curve with it",
        playback.eq_response().iter().all(|db| db.abs() < 0.001),
        format!(
            "max {:.4}dB while the sliders still read {:+.0}/{:+.0}",
            playback
                .eq_response()
                .iter()
                .fold(0.0f32, |m, db| m.max(db.abs())),
            playback.eq().gains_db[LOW_BAND],
            playback.eq().preamp_db
        ),
    );

    // --- what the UI reads back ---

    let wild = EqSettings {
        enabled: true,
        preamp_db: 99.0,
        gains_db: [99.0, -99.0, f32::NAN, 0.0, 0.0, 6.0, 0.0, 0.0, 0.0, 0.0],
    };
    playback.set_eq(wild);
    let held = playback.eq();
    check(
        "settings are clamped before the engine keeps them",
        held.preamp_db == 12.0
            && held.gains_db[0] == 12.0
            && held.gains_db[1] == -12.0
            && held.gains_db[2] == 0.0
            && held.gains_db[5] == 6.0,
        format!(
            "preamp {:+.0}, sliders {:+.0}/{:+.0}/{:+.0}",
            held.preamp_db, held.gains_db[0], held.gains_db[1], held.gains_db[2]
        ),
    );

    // --- a seek does not lose the setting ---

    let sweep = settings_with(LOW_BAND, 12.0);
    playback.set_eq(sweep);
    playback.seek(10.0);
    sleep(SETTLE);
    let after_seek = playback.spectrum();
    check(
        "the EQ survives a seek",
        ((after_seek[low] - flat[low]) - 12.0).abs() < 1.5,
        format!(
            "{:+.2}dB at {:.0}Hz, {:.2}s into the track",
            after_seek[low] - flat[low],
            bands[low],
            playback.position_seconds()
        ),
    );

    // --- the axis itself ---

    let frequencies = response_frequencies();
    check(
        "the curve's axis is log-spaced across the audible band",
        frequencies.len() == player_core::eq::RESPONSE_POINTS
            && (frequencies[0] - 20.0).abs() < 0.01
            && (frequencies[frequencies.len() - 1] - 20_000.0).abs() < 1.0
            && frequencies.windows(2).all(|w| w[1] > w[0])
            && BAND_FREQUENCIES.len() == player_core::EQ_BANDS,
        format!(
            "{} points, {:.0}Hz..{:.0}Hz, for {} sliders at {:.2}Hz..{:.0}Hz",
            frequencies.len(),
            frequencies[0],
            frequencies[frequencies.len() - 1],
            BAND_FREQUENCIES.len(),
            BAND_FREQUENCIES[0],
            BAND_FREQUENCIES[player_core::EQ_BANDS - 1]
        ),
    );

    drop(playback);
    let _ = std::fs::remove_file(&path);
}
