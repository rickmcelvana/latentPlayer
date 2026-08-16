//! Live check of the loudness and phase meters (Phase 1 M5). Synthesises its
//! own WAVs, so it needs no fixture and no arguments:
//!
//!   cargo run -p player-core --example loudness_smoke
//!
//! The native counterpart of `/loudness.html` in the web build, running the
//! same checks against the same claims so the two shells can be held to one
//! standard -- the arrangement that caught M4's band-edge bug, where the two
//! disagreed about identical audio.
//!
//! It opens the real default output device and makes real (quiet) sound,
//! because what is being tested is the whole path -- decoder thread, ring,
//! callback, tap, analysis thread -- not the DSP, which the unit tests in
//! `loudness.rs` and `phase.rs` already pin down against analytic answers.
//! Every check here has an arithmetic answer: a tone generated at -23 LUFS must
//! read -23 LUFS, a channel inverted against its partner must read a
//! correlation of -1, and a track change must start the integrated measurement
//! over rather than averaging across it.

use std::io::Write;
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

use player_core::loudness::{FLOOR_DBTP, FLOOR_LUFS};
use player_core::{LocalFile, Playback};

const SRC_RATE: u32 = 44_100;
const TONE_SECONDS: u32 = 10;
const TONE_HZ: f32 = 1000.0;

/// The two tracks' loudness, in LUFS. -23 is EBU R128's delivery target; the
/// second is 10 LU quieter so that "did the integrated measurement start over
/// at the hand-off" has an answer 7 LU away from the one a meter that averaged
/// across the seam would give.
const FIRST_LUFS: f32 = -23.0;
const SECOND_LUFS: f32 = -33.0;

/// Amplitude of a sine that measures `lufs` when it is in both channels.
///
/// BS.1770-4 sums the weighted channels, so identical channels are 3.01 LU
/// louder than one alone; a sine's RMS is its amplitude over sqrt(2). Both
/// shells generate their tones from this same arithmetic.
fn amplitude_for(lufs: f32) -> f32 {
    2f32.sqrt() * 10f32.powf((lufs - 3.01) / 20.0)
}

fn check(label: &str, ok: bool, detail: String) {
    println!("  [{}] {label}: {detail}", if ok { "PASS" } else { "FAIL" });
}

/// A 16-bit stereo WAV of one sine, with the right channel scaled by
/// `right_gain` (-1.0 inverts it). Same signal the web harness generates.
fn write_wav(path: &Path, amplitude: f32, right_gain: f32) -> std::io::Result<()> {
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
        let t = 2.0 * std::f32::consts::PI * TONE_HZ * i as f32 / SRC_RATE as f32;
        let left = (t.sin() * amplitude * 32_767.0) as i16;
        let right = (t.sin() * amplitude * right_gain * 32_767.0) as i16;
        out.write_all(&left.to_le_bytes())?;
        out.write_all(&right.to_le_bytes())?;
    }
    out.flush()
}

/// How far the goniometer's points stray off an axis, as a fraction of full
/// scale. Mono material has no side component and an inverted pair has no mid
/// one, so one coordinate of every point should be ~0.
fn off_axis(points: &[f32], coordinate: usize) -> f32 {
    points
        .chunks_exact(2)
        .map(|pair| pair[coordinate].abs())
        .fold(0.0f32, f32::max)
}

fn main() {
    let dir = std::env::temp_dir();
    let first = dir.join("latentplayer_loudness_first.wav");
    let second = dir.join("latentplayer_loudness_second.wav");
    write_wav(&first, amplitude_for(FIRST_LUFS), 1.0).expect("write first tone");
    // Inverted right channel: quieter *and* out of phase, so one track change
    // exercises both the phase meters and the integrated reset.
    write_wav(&second, amplitude_for(SECOND_LUFS), -1.0).expect("write second tone");

    let playback = Playback::open(Box::new(LocalFile::new(&first))).expect("open first tone");
    playback.set_volume(0.1); // be kind to whoever is at the keyboard
    println!(
        "output:  {} Hz, {} ch; tones at {FIRST_LUFS} and {SECOND_LUFS} LUFS",
        playback.output_sample_rate(),
        playback.output_channels()
    );

    let idle = playback.meters();
    check(
        "nothing measured yet reads the floor",
        idle.momentary_lufs <= FLOOR_LUFS
            && idle.integrated_lufs <= FLOOR_LUFS
            && idle.true_peak_db <= FLOOR_DBTP,
        format!(
            "M {:.1}, I {:.1} LUFS, TP {:.1} dBTP",
            idle.momentary_lufs, idle.integrated_lufs, idle.true_peak_db
        ),
    );

    playback.play();
    // Long enough for the 3s short-term window to fill.
    sleep(Duration::from_millis(4000));

    let m = playback.meters();
    check(
        "the tone reads the loudness it was generated at",
        (m.momentary_lufs - FIRST_LUFS).abs() < 0.5,
        format!("{:.2} LUFS momentary, want {FIRST_LUFS}", m.momentary_lufs),
    );
    check(
        "short-term agrees with momentary on steady material",
        (m.short_term_lufs - m.momentary_lufs).abs() < 0.3,
        format!(
            "{:.2} short-term against {:.2} momentary",
            m.short_term_lufs, m.momentary_lufs
        ),
    );
    check(
        "integrated agrees with them",
        (m.integrated_lufs - FIRST_LUFS).abs() < 0.5,
        format!("{:.2} LUFS integrated", m.integrated_lufs),
    );
    // A sine's peak is its amplitude, and `amplitude_for` puts that at exactly
    // the target loudness in dBFS for this signal -- so the true peak lands on
    // the same number by arithmetic rather than by coincidence.
    check(
        "true peak reads the tone's own peak",
        (m.true_peak_db - FIRST_LUFS).abs() < 0.5,
        format!("{:.2} dBTP, want {FIRST_LUFS}", m.true_peak_db),
    );
    check(
        "identical channels read a correlation of +1",
        (m.correlation - 1.0).abs() < 0.02,
        format!("{:.3}", m.correlation),
    );

    let points = playback.goniometer();
    check(
        "the goniometer stands mono material up the centre",
        !points.is_empty() && off_axis(&points, 0) < 0.02,
        format!(
            "{} points, {:.4} of side energy at most",
            points.len() / 2,
            off_axis(&points, 0)
        ),
    );

    // The callback taps the ring *before* it applies gain, so the meters read
    // the programme rather than the monitoring level -- the only sane answer
    // for a loudness meter, and the same place the web worklet taps.
    playback.set_volume(0.5);
    sleep(Duration::from_millis(600));
    let louder = playback.meters();
    check(
        "the volume control does not move the meters",
        (louder.momentary_lufs - m.momentary_lufs).abs() < 0.3,
        format!(
            "{:.2} LUFS at volume 0.1, {:.2} at 0.5",
            m.momentary_lufs, louder.momentary_lufs
        ),
    );
    playback.set_volume(0.1);

    // A gapless hand-off into the quieter, out-of-phase track. The audio is
    // continuous across the seam -- that is the point of M3 -- so nothing in
    // the tap marks it, and the meters only reset because the analysis thread
    // watches `track_index` (M5 decision 2).
    playback.set_next(Box::new(LocalFile::new(&second)));
    playback.seek(TONE_SECONDS as f64 - 0.3);
    sleep(Duration::from_millis(4000));

    let after = playback.meters();
    check(
        "an inverted channel reads a correlation of -1",
        (after.correlation + 1.0).abs() < 0.02,
        format!("{:.3}", after.correlation),
    );
    check(
        "the goniometer lays an out-of-phase pair flat",
        off_axis(&playback.goniometer(), 1) < 0.02,
        format!(
            "{:.4} of mid energy at most",
            off_axis(&playback.goniometer(), 1)
        ),
    );
    check(
        "the new track's momentary loudness is its own",
        (after.momentary_lufs - SECOND_LUFS).abs() < 0.5,
        format!("{:.2} LUFS, want {SECOND_LUFS}", after.momentary_lufs),
    );
    // Averaged across the seam instead of restarted, this would read about
    // -25.6: the mean power of four seconds at -23 and four at -33.
    check(
        "integrated starts over at the hand-off rather than averaging across it",
        (after.integrated_lufs - SECOND_LUFS).abs() < 0.5,
        format!(
            "{:.2} LUFS integrated ({:.2} would mean it averaged the two tracks)",
            after.integrated_lufs, -25.6
        ),
    );
    check(
        "true peak restarts with it",
        (after.true_peak_db - SECOND_LUFS).abs() < 0.5,
        format!("{:.2} dBTP, want {SECOND_LUFS}", after.true_peak_db),
    );

    // Pausing sends the device silence, and the tap records it. The momentary
    // window empties within 400ms; the integrated number must *not* follow it
    // down, because BS.1770-4's absolute gate exists precisely so that silence
    // between the music doesn't count as quiet music.
    let before_pause = playback.meters();
    playback.pause();
    sleep(Duration::from_millis(1000));
    let paused = playback.meters();
    check(
        "a pause empties the momentary window",
        paused.momentary_lufs <= FLOOR_LUFS,
        format!(
            "{:.1} -> {:.1} LUFS",
            before_pause.momentary_lufs, paused.momentary_lufs
        ),
    );
    check(
        "the gate keeps that silence out of the integrated number",
        (paused.integrated_lufs - before_pause.integrated_lufs).abs() < 0.3,
        format!(
            "{:.2} -> {:.2} LUFS across a second of silence",
            before_pause.integrated_lufs, paused.integrated_lufs
        ),
    );

    drop(playback);
    let _ = std::fs::remove_file(&first);
    let _ = std::fs::remove_file(&second);
}
