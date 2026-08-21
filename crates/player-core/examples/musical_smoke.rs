//! Live check of the musical intelligence (Phase 3 M5). Synthesises its own
//! WAVs, so it needs no fixture and no arguments:
//!
//!   cargo run -p player-core --example musical_smoke
//!
//! The native counterpart of `/musical.html` in the web build, running the same
//! checks against the same claims so the two shells can be held to one
//! standard -- the arrangement that caught M4's band-edge bug, where the two
//! disagreed about identical audio.
//!
//! It opens the real default output device and makes real (quiet) sound,
//! because what is being tested is the whole path -- decoder thread, ring,
//! callback, tap, analysis thread, snapshot -- not the arithmetic, which the
//! unit tests in `onset.rs` and `tempo.rs` already pin down against analytic
//! answers.
//!
//! Every check here has a known answer rather than a plausible one, which is
//! what this milestone's verification standard asks for: a click track
//! synthesised at 120 BPM must read 120, one at 90 must read 90 (a tempo no
//! integer autocorrelation lag can express), an offbeat on every eighth must
//! not double the reading, material with no steady pulse must say so, and a
//! track change must start the estimate over rather than carrying the previous
//! track's tempo into it.

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

use player_core::tempo::NO_TEMPO;
use player_core::{LocalFile, Playback};

const SRC_RATE: u32 = 44_100;
const TRACK_SECONDS: u32 = 30;

/// The two click tracks' tempi. 120 is the tempo prior's own centre; 90 is well
/// off it *and* falls between autocorrelation lags (66.67 at a 100Hz detection
/// function), so a tracker that reported its prior, or one that reported an
/// integer lag, fails one of the two.
const FIRST_BPM: f32 = 120.0;
const SECOND_BPM: f32 = 90.0;

/// How long the pad takes to reach full level. See `Fixture::Pad`.
const PAD_FADE_SECONDS: f32 = 1.0;

/// The tone used for the brightness check, and for "a held note is not a beat".
const TONE_HZ: f32 = 1000.0;

/// How long to let a tempo settle before reading it: the tracker's warm-up is
/// four seconds and its autocorrelation window is eight, so nine seconds is a
/// full window plus the time for one refresh.
const SETTLE: Duration = Duration::from_secs(9);

/// How close a reading has to be to the tempo the fixture was generated at.
const BPM_TOLERANCE: f32 = 0.5;

static PASSED: AtomicUsize = AtomicUsize::new(0);
static RUN: AtomicUsize = AtomicUsize::new(0);

fn check(label: &str, ok: bool, detail: String) {
    RUN.fetch_add(1, Ordering::Relaxed);
    if ok {
        PASSED.fetch_add(1, Ordering::Relaxed);
    }
    println!("  [{}] {label}: {detail}", if ok { "PASS" } else { "FAIL" });
}

/// The pseudo-random source both shells use for their noise bursts. A plain
/// LCG, so that the click track this file writes and the one `musicalSmoke.ts`
/// writes are the same audio sample for sample -- which is what makes the two
/// shells' figures comparable at all.
struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (self.0 >> 8) as f32 / 8_388_608.0 - 1.0
    }
}

/// What a fixture sounds like.
enum Fixture {
    /// Broadband bursts at `bpm`, optionally with a weaker one on every eighth
    /// -- the octave-error case, which is what a hi-hat under a snare does to a
    /// tempo estimator that only looks for the strongest periodicity.
    Clicks { bpm: f32, offbeat: bool },
    /// A held sine: brightness with a known answer, and no beat at all.
    Tone,
    /// A sustained pad: swelling partials, no attacks anywhere in it.
    ///
    /// The fixture M6's first live look earned. Sustained organ material was
    /// reported as a confident 142 BPM with `transient 1.000` -- because the
    /// transient figure was normalised by its own recent peak alone, so flux
    /// thirty times too small to be an attack still read as full strength, and
    /// the swells crossed the onset picker's bar often enough to correlate
    /// (2026-08-20). There is nothing here for either figure to find.
    Pad,
    /// Soft notes at deliberately irregular intervals -- the rubato case. There
    /// are onsets to find and no tempo to find, which is the failure this
    /// milestone most needs to fail *honestly* (decision 36).
    Rubato,
}

/// Frames between clicks at `bpm`. Rounded, so both shells place them
/// identically.
fn click_period(bpm: f32) -> usize {
    (60.0 / bpm * SRC_RATE as f32).round() as usize
}

/// One fixture as mono samples. Both shells generate this from the same
/// arithmetic; the web half is `fixture()` in musicalSmoke.ts.
fn render(fixture: &Fixture, frames: usize) -> Vec<f32> {
    let mut noise = Noise(2_463_534_242);
    match fixture {
        Fixture::Clicks { bpm, offbeat } => {
            let period = click_period(*bpm);
            let half = period / 2;
            (0..frames)
                .map(|n| {
                    let into = n % period;
                    // 128 frames of decaying noise: about 3ms, short enough to
                    // be one onset and broad enough to move every bin.
                    if into < 128 {
                        noise.next() * (1.0 - into as f32 / 128.0) * 0.7
                    } else if *offbeat && into >= half && into < half + 128 {
                        let into = into - half;
                        noise.next() * (1.0 - into as f32 / 128.0) * 0.35
                    } else {
                        0.0
                    }
                })
                .collect()
        }
        Fixture::Tone => (0..frames)
            .map(|n| {
                0.5 * (2.0 * std::f32::consts::PI * TONE_HZ * n as f32 / SRC_RATE as f32).sin()
            })
            .collect(),
        Fixture::Pad => {
            // Four partials, slightly detuned so they beat against each other,
            // under a slow swell. Every ingredient of the material that was
            // misread: continuous, harmonically rich, and moving -- with no
            // event in it anywhere.
            (0..frames)
                .map(|n| {
                    let t = n as f32 / SRC_RATE as f32;
                    let swell = 0.5 + 0.5 * (2.0 * std::f32::consts::PI * 0.07 * t).sin();
                    let voices: f32 = [(110.0, 0.5), (164.8, 0.35), (220.5, 0.3), (330.0, 0.2)]
                        .iter()
                        .map(|(hz, amp)| amp * (2.0 * std::f32::consts::PI * hz * t).sin())
                        .sum();
                    // Faded in over a second, because a file that begins at
                    // half amplitude on its first sample *does* begin with an
                    // attack -- and the detector is right to say so. Without
                    // this the fixture's only transient is the fixture's own
                    // start, which is not what it is here to measure.
                    let fade = (t / PAD_FADE_SECONDS).min(1.0);
                    0.35 * fade * swell * voices
                })
                .collect()
        }
        Fixture::Rubato => {
            // Onsets in seconds, and the note each starts. Irregular on
            // purpose: no two gaps are the same, and no gap is a multiple of
            // another, so there is no tempo here to find.
            const NOTES: [(f32, f32); 12] = [
                (0.0, 220.0),
                (0.71, 277.2),
                (1.13, 329.6),
                (2.02, 261.6),
                (2.35, 392.0),
                (3.4, 220.0),
                (3.79, 293.7),
                (4.9, 349.2),
                (5.22, 246.9),
                (6.4, 196.0),
                (7.05, 311.1),
                (8.3, 261.6),
            ];
            let mut out = vec![0.0f32; frames];
            let loop_seconds = 9.5;
            let loop_frames = (loop_seconds * SRC_RATE as f32) as usize;
            for start in (0..frames).step_by(loop_frames) {
                for (at, hz) in NOTES {
                    let onset = start + (at * SRC_RATE as f32) as usize;
                    for i in 0..(SRC_RATE as usize) {
                        let Some(slot) = out.get_mut(onset + i) else {
                            break;
                        };
                        let t = i as f32 / SRC_RATE as f32;
                        // A 30ms attack and a slow decay: an onset a detector
                        // can find, with none of a drum's sharpness.
                        let attack = (t / 0.03).min(1.0);
                        let decay = (-t * 2.5).exp();
                        *slot +=
                            0.35 * attack * decay * (2.0 * std::f32::consts::PI * hz * t).sin();
                    }
                }
            }
            out
        }
    }
}

/// A 16-bit stereo WAV of `fixture`, identical in both shells.
fn write_wav(path: &Path, fixture: &Fixture) -> std::io::Result<()> {
    let frames = (SRC_RATE * TRACK_SECONDS) as usize;
    let mono = render(fixture, frames);
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
    for sample in &mono {
        let value = (sample.clamp(-1.0, 1.0) * 32_767.0) as i16;
        out.write_all(&value.to_le_bytes())?;
        out.write_all(&value.to_le_bytes())?;
    }
    out.flush()
}

/// What was seen while the music played: onsets cannot be read after the fact,
/// because each one is published for as long as it takes the next drain to
/// replace it (decision 37).
struct Watched {
    onsets: usize,
    /// Beat phases sampled at the moment an onset was reported.
    phase_at_onsets: Vec<f32>,
    lowest_phase: f32,
    highest_phase: f32,
    /// The brightest window seen. A click track's *newest* window is silence
    /// most of the time -- the clicks are 3ms apart in a 500ms beat -- so the
    /// centroid of a percussive track is a thing you catch, not a thing you
    /// read afterwards.
    brightest: f32,
    /// The strongest transient seen. What M6's live look turned into a figure:
    /// on material with no attacks this has to stay near zero, and it read
    /// 1.000 until `onset.rs` was given an absolute reference to measure
    /// against.
    strongest_transient: f32,
}

/// Plays for `duration`, polling faster than the analysis publishes so that
/// onsets are seen rather than stepped over.
fn watch(playback: &Playback, duration: Duration) -> Watched {
    const POLL: Duration = Duration::from_millis(10);
    let until = Instant::now() + duration;
    let mut watched = Watched {
        onsets: 0,
        phase_at_onsets: Vec::new(),
        lowest_phase: f32::MAX,
        highest_phase: f32::MIN,
        brightest: 0.0,
        strongest_transient: 0.0,
    };
    let mut last_onset = 0.0f32;
    while Instant::now() < until {
        let musical = playback.musical();
        if musical.onset > 0.0 && last_onset == 0.0 {
            watched.onsets += 1;
            if musical.bpm > NO_TEMPO {
                watched.phase_at_onsets.push(musical.beat_phase);
            }
        }
        last_onset = musical.onset;
        watched.brightest = watched.brightest.max(musical.centroid_hz);
        watched.strongest_transient = watched.strongest_transient.max(musical.transient);
        if musical.bpm > NO_TEMPO {
            watched.lowest_phase = watched.lowest_phase.min(musical.beat_phase);
            watched.highest_phase = watched.highest_phase.max(musical.beat_phase);
        }
        sleep(POLL);
    }
    watched
}

/// How far a phase is from the nearest beat, in beats: 0.0 is on it, 0.5 is as
/// far away as it is possible to be.
fn off_beat(phase: f32) -> f32 {
    phase.min(1.0 - phase)
}

fn main() {
    let dir = std::env::temp_dir();
    let first = dir.join("latentplayer_musical_120.wav");
    let second = dir.join("latentplayer_musical_90.wav");
    let offbeat = dir.join("latentplayer_musical_offbeat.wav");
    let tone = dir.join("latentplayer_musical_tone.wav");
    let rubato = dir.join("latentplayer_musical_rubato.wav");
    let pad = dir.join("latentplayer_musical_pad.wav");
    write_wav(
        &first,
        &Fixture::Clicks {
            bpm: FIRST_BPM,
            offbeat: false,
        },
    )
    .expect("write the 120 BPM click track");
    write_wav(
        &second,
        &Fixture::Clicks {
            bpm: SECOND_BPM,
            offbeat: false,
        },
    )
    .expect("write the 90 BPM click track");
    write_wav(
        &offbeat,
        &Fixture::Clicks {
            bpm: FIRST_BPM,
            offbeat: true,
        },
    )
    .expect("write the offbeat click track");
    write_wav(&tone, &Fixture::Tone).expect("write the tone");
    write_wav(&rubato, &Fixture::Rubato).expect("write the rubato notes");
    write_wav(&pad, &Fixture::Pad).expect("write the pad");

    let playback = Playback::open(Box::new(LocalFile::new(&first))).expect("open the click track");
    playback.set_volume(0.1); // be kind to whoever is at the keyboard
    println!(
        "output:  {} Hz, {} ch; click tracks at {FIRST_BPM} and {SECOND_BPM} BPM",
        playback.output_sample_rate(),
        playback.output_channels()
    );

    let idle = playback.musical();
    check(
        "nothing measured yet reports no tempo",
        idle.bpm == NO_TEMPO && idle.beat_confidence == 0.0 && idle.onset == 0.0,
        format!(
            "{:.1} BPM, confidence {:.2}",
            idle.bpm, idle.beat_confidence
        ),
    );

    playback.play();
    let watched = watch(&playback, SETTLE);

    let m = playback.musical();
    check(
        "a click track reads the tempo it was generated at",
        (m.bpm - FIRST_BPM).abs() < BPM_TOLERANCE,
        format!("{:.2} BPM, want {FIRST_BPM}", m.bpm),
    );
    check(
        "and says it is sure of it",
        m.beat_confidence > 0.5,
        format!("confidence {:.2}", m.beat_confidence),
    );
    // Nine seconds at 120 BPM is eighteen clicks, minus whatever the ring and
    // the tap were still filling when play() returned.
    check(
        "every click is an onset, and no click is two",
        (14..=18).contains(&watched.onsets),
        format!("{} onsets over {}s", watched.onsets, SETTLE.as_secs()),
    );
    // The last few, not all of them: the phase is a loop that locks, and the
    // first onsets after a tempo is established are the ones it is locking
    // *with* (decision 35). Locked, it should be within a tenth of a beat --
    // which is a tenth of 500ms here, and includes the drain this was polled
    // through.
    let locked: Vec<f32> = watched
        .phase_at_onsets
        .iter()
        .copied()
        .rev()
        .take(5)
        .collect();
    let worst = locked
        .iter()
        .copied()
        .fold(0.0f32, |worst, phase| worst.max(off_beat(phase)));
    check(
        "the beat phase is where the beat is, once it has locked",
        locked.len() >= 3 && worst < 0.15,
        format!(
            "{:.3} of a beat out at worst over the last {} of {} onsets",
            worst,
            locked.len(),
            watched.phase_at_onsets.len()
        ),
    );
    check(
        "and it sweeps the whole beat rather than sticking",
        watched.lowest_phase < 0.25 && watched.highest_phase > 0.75,
        format!(
            "phase spanned {:.2}..{:.2}",
            watched.lowest_phase, watched.highest_phase
        ),
    );
    check(
        "a click's own window is broadband",
        watched.brightest > 2000.0,
        format!("{:.0} Hz at brightest", watched.brightest),
    );

    // Silence is not music: the onsets stop immediately, because the detection
    // function has nothing to detect.
    playback.pause();
    sleep(Duration::from_millis(400));
    let paused = playback.musical();
    check(
        "a pause stops the onsets",
        paused.onset == 0.0,
        format!("onset {:.3} while paused", paused.onset),
    );
    playback.play();

    // A gapless hand-off into the slower track. The audio is continuous across
    // the seam -- that is the point of Phase 1 M3 -- so nothing in the tap marks
    // it, and the tempo only starts over because the analysis thread watches
    // `track_index` (decision 31).
    playback.set_next(Box::new(LocalFile::new(&second)));
    playback.seek(TRACK_SECONDS as f64 - 0.3);
    sleep(SETTLE);

    let after = playback.musical();
    check(
        "the new track's tempo is its own, not the last one's",
        (after.bpm - SECOND_BPM).abs() < BPM_TOLERANCE,
        format!(
            "{:.2} BPM, want {SECOND_BPM} ({FIRST_BPM} would mean it carried across the seam)",
            after.bpm
        ),
    );
    // 90 BPM is lag 66.67 at a 100Hz detection function, and the integer lags
    // either side of it are 89.55 and 90.91 -- so this figure is also the check
    // that the autocorrelation peak is interpolated (decision 34).
    check(
        "a tempo between two lags is still reported exactly",
        (after.bpm - SECOND_BPM).abs() < BPM_TOLERANCE,
        format!(
            "{:.2} BPM, against 89.55 and 90.91 either side of it",
            after.bpm
        ),
    );
    drop(playback);

    // The octave error, which is what every naive tempo estimator does with a
    // hi-hat: something on every eighth, half the strength of the beat.
    let playback = Playback::open(Box::new(LocalFile::new(&offbeat))).expect("open the offbeat");
    playback.set_volume(0.1);
    playback.play();
    sleep(SETTLE);
    let eighths = playback.musical();
    check(
        "an offbeat on every eighth does not double the tempo",
        (eighths.bpm - FIRST_BPM).abs() < 1.0,
        format!(
            "{:.2} BPM, want {FIRST_BPM} (240 or 60 would be an octave error)",
            eighths.bpm
        ),
    );
    drop(playback);

    // A held tone: a brightness with an arithmetic answer, and no beat at all.
    let playback = Playback::open(Box::new(LocalFile::new(&tone))).expect("open the tone");
    playback.set_volume(0.1);
    playback.play();
    sleep(SETTLE);
    let held = playback.musical();
    check(
        "brightness reads the tone's own frequency",
        (held.centroid_hz - TONE_HZ).abs() < 100.0,
        format!("{:.0} Hz, want {TONE_HZ}", held.centroid_hz),
    );
    check(
        "a held note is not a beat",
        held.bpm == NO_TEMPO,
        format!(
            "{:.1} BPM, confidence {:.2}",
            held.bpm, held.beat_confidence
        ),
    );
    drop(playback);

    // Rubato: onsets to find, and no tempo to find. The honest failure.
    let playback = Playback::open(Box::new(LocalFile::new(&rubato))).expect("open the rubato");
    playback.set_volume(0.1);
    playback.play();
    let watched = watch(&playback, SETTLE);
    let free = playback.musical();
    check(
        "material with no steady pulse is not reported as confident",
        free.beat_confidence < 0.5,
        format!(
            "confidence {:.2} at {:.1} BPM",
            free.beat_confidence, free.bpm
        ),
    );
    check(
        "but its onsets are still found",
        watched.onsets >= 6,
        format!("{} onsets over {}s", watched.onsets, SETTLE.as_secs()),
    );
    check(
        "and its notes are darker than a click",
        watched.brightest > 0.0 && watched.brightest < 2000.0,
        format!("{:.0} Hz at brightest", watched.brightest),
    );
    drop(playback);

    // The pad, which is where M6's live look found this suite believing itself.
    // Sustained material with no attacks must report no tempo and no transient
    // -- the second is the cause and the first is the symptom, so both are
    // checked rather than only the one a listener noticed.
    let playback = Playback::open(Box::new(LocalFile::new(&pad))).expect("open the pad");
    playback.set_volume(0.1);
    playback.play();
    let watched = watch(&playback, SETTLE);
    let held = playback.musical();
    check(
        "a sustained pad is not a beat",
        held.bpm == NO_TEMPO && held.beat_confidence == 0.0,
        format!(
            "{:.1} BPM, confidence {:.2} (142 BPM at 1.00 was the bug)",
            held.bpm, held.beat_confidence
        ),
    );
    check(
        "and nothing in it reads as a transient",
        watched.strongest_transient < 0.25,
        format!(
            "{:.3} at strongest, against 1.000 before the reference was absolute",
            watched.strongest_transient
        ),
    );
    drop(playback);

    for path in [&first, &second, &offbeat, &tone, &rubato, &pad] {
        let _ = std::fs::remove_file(path);
    }

    println!(
        "
{}/{} checks passed",
        PASSED.load(Ordering::Relaxed),
        RUN.load(Ordering::Relaxed)
    );
}
