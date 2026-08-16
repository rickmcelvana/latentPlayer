//! Live check of the analysis tap and spectrum analyser (Phase 1 M4).
//! Synthesises its own WAVs, so it needs no fixture and no arguments:
//!
//!   cargo run -p player-core --example spectrum_smoke
//!
//! The native counterpart of `/spectrum.html` in the web build, running the
//! same checks against the same claims so the two shells can be held to one
//! standard.
//!
//! It opens the real default output device and makes real (quiet) sound,
//! because what is being tested is the whole path -- decoder thread, ring,
//! callback, tap, analysis thread -- not the FFT, which the unit tests in
//! `spectrum.rs` already pin down against analytic answers. Every check here
//! has an arithmetic answer rather than a plausible-looking picture: a -3dBFS
//! sine at 440Hz must show up in the band containing 440Hz, at -3dBFS, and
//! nowhere else.

use std::io::Write;
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

use player_core::{LocalFile, Playback};

const SRC_RATE: u32 = 44_100;
const TONE_SECONDS: u32 = 8;
const LOW_HZ: f32 = 440.0;
const HIGH_HZ: f32 = 3520.0;

/// Linear amplitude of the test tones: -3.1dBFS. Deliberately not full scale,
/// so "did the level survive the whole path" has an answer that a stuck
/// normalisation wouldn't also produce.
const TONE_AMPLITUDE: f32 = 0.7;

fn tone_dbfs() -> f32 {
    20.0 * TONE_AMPLITUDE.log10()
}

fn check(label: &str, ok: bool, detail: String) {
    println!("  [{}] {label}: {detail}", if ok { "PASS" } else { "FAIL" });
}

/// A 16-bit stereo WAV of one sine. Same signal the web harness generates.
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
        let sample = (t.sin() * TONE_AMPLITUDE * 32_767.0) as i16;
        out.write_all(&sample.to_le_bytes())?;
        out.write_all(&sample.to_le_bytes())?;
    }
    out.flush()
}

/// Index of the band whose centre is nearest `hz`, measured on the log scale
/// the bands are spaced on.
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

fn loudest(levels: &[f32]) -> usize {
    levels
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(i, _)| i)
        .unwrap_or(0)
}

fn main() {
    let dir = std::env::temp_dir();
    let low = dir.join("latentplayer_spectrum_low.wav");
    let high = dir.join("latentplayer_spectrum_high.wav");
    write_wav(&low, TONE_SECONDS, LOW_HZ).expect("write low tone");
    write_wav(&high, TONE_SECONDS, HIGH_HZ).expect("write high tone");

    let playback = Playback::open(Box::new(LocalFile::new(&low))).expect("open low tone");
    playback.set_volume(0.1); // be kind to whoever is at the keyboard
    let bands = playback.spectrum_band_centres().to_vec();
    println!(
        "output:  {} Hz, {} bands, tones at {:.1} dBFS",
        playback.output_sample_rate(),
        bands.len(),
        tone_dbfs()
    );

    check(
        "the axis is published with the engine",
        bands.len() == player_core::BAND_COUNT
            && bands[0] >= 20.0
            && *bands.last().expect("bands") < playback.output_sample_rate() as f32 / 2.0
            && bands.windows(2).all(|w| w[1] > w[0]),
        format!(
            "{} bands, {:.1}Hz..{:.0}Hz, ascending",
            bands.len(),
            bands[0],
            bands.last().expect("bands")
        ),
    );

    let idle = playback.spectrum();
    check(
        "nothing measured yet reads the floor, not full scale",
        idle.len() == bands.len() && idle.iter().all(|&db| db <= player_core::spectrum::FLOOR_DB),
        format!(
            "max {:.1}dB across {} bands",
            idle.iter().copied().fold(f32::MIN, f32::max),
            idle.len()
        ),
    );

    playback.play();
    sleep(Duration::from_millis(600));

    let levels = playback.spectrum();
    let want_low = band_at(&bands, LOW_HZ);
    check(
        "the tone lands in the band that holds it",
        loudest(&levels) == want_low,
        format!(
            "loudest band {} ({:.0}Hz), want {want_low} ({:.0}Hz)",
            loudest(&levels),
            bands[loudest(&levels)],
            bands[want_low]
        ),
    );
    check(
        "it reads the level it was generated at",
        (levels[want_low] - tone_dbfs()).abs() < 2.0,
        format!("{:.2}dBFS, want {:.2}dBFS", levels[want_low], tone_dbfs()),
    );

    // Two octaves down is 12 bands away at 6 bands/octave -- well outside the
    // window's skirt, so anything there is leakage or a broken window.
    let far = band_at(&bands, LOW_HZ / 4.0);
    check(
        "energy stays where it belongs",
        levels[far] < levels[want_low] - 40.0,
        format!(
            "{:.1}dB two octaves down against a {:.1}dB peak",
            levels[far], levels[want_low]
        ),
    );

    // The callback taps the ring *before* it applies gain, so the meters read
    // the program material rather than the monitoring level -- which is the
    // only sane answer for a loudness meter, and matches where the web
    // worklet taps (before its GainNode).
    playback.set_volume(0.5);
    sleep(Duration::from_millis(300));
    let louder = playback.spectrum();
    check(
        "the volume control does not move the meters",
        (louder[want_low] - levels[want_low]).abs() < 1.5,
        format!(
            "{:.2}dBFS at volume 0.1, {:.2}dBFS at 0.5",
            levels[want_low], louder[want_low]
        ),
    );
    playback.set_volume(0.1);

    // A different tone, three octaves up: the display has to follow the audio
    // rather than hold the first picture it drew.
    playback.set_next(Box::new(LocalFile::new(&high)));
    playback.seek(TONE_SECONDS as f64 - 0.3);
    // Long enough for the old tone's band to decay well past the 40dB the
    // check below wants: at 30dB/s that takes over a second, and the web
    // harness gets there for free by loading a new file (which resets the
    // levels) where this one hands over gaplessly.
    sleep(Duration::from_millis(2500));
    let after = playback.spectrum();
    let want_high = band_at(&bands, HIGH_HZ);
    check(
        "a different tone moves the peak",
        loudest(&after) == want_high && (after[want_high] - tone_dbfs()).abs() < 2.0,
        format!(
            "loudest band {} ({:.0}Hz) at {:.2}dBFS, want {want_high} ({:.0}Hz)",
            loudest(&after),
            bands[loudest(&after)],
            after[want_high],
            bands[want_high]
        ),
    );
    check(
        "the band the old tone was in has gone quiet",
        after[want_low] < after[want_high] - 40.0,
        format!("{:.1}dB at {:.0}Hz", after[want_low], bands[want_low]),
    );

    // Pausing stops the audio but not the meters: the tap records the silence
    // the device is being sent, so the display decays at the release rate
    // instead of freezing on the last frame before the pause.
    let before_pause = playback.spectrum()[want_high];
    playback.pause();
    sleep(Duration::from_millis(1000));
    let paused = playback.spectrum()[want_high];
    check(
        "a silent input decays rather than freezing",
        paused < before_pause - 20.0,
        format!("{before_pause:.1}dB -> {paused:.1}dB over ~1s (release is 30dB/s)"),
    );

    // A seek clears the tap under the analysis thread; the meters have to come
    // back rather than latch whatever the discontinuity did to them.
    playback.seek(2.0);
    playback.play();
    sleep(Duration::from_millis(700));
    let resumed = playback.spectrum();
    check(
        "the meters recover after a seek",
        loudest(&resumed) == want_high && (resumed[want_high] - tone_dbfs()).abs() < 2.0,
        format!(
            "{:.2}dBFS in band {} at {:.2}s",
            resumed[want_high],
            loudest(&resumed),
            playback.position_seconds()
        ),
    );

    drop(playback);
    let _ = std::fs::remove_file(&low);
    let _ = std::fs::remove_file(&high);
}
