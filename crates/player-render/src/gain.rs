//! Display gain: how much to magnify the signal for the eye (Phase 3 M1, added
//! 2026-08-18 after the first live fullscreen run).
//!
//! # Why a scope needs this at all
//!
//! The vectorscope's frame is sized for a *full-scale* signal -- a full-scale
//! mono tone stands 90% of the way up it, and nothing a signal can do reaches
//! the edge (see `visuals::vectorscope`). That is the right frame for a
//! measurement and the wrong one for looking at music: a master peaking at
//! -12 dBFS is a quarter of full scale, so it draws at about 22% of the height,
//! and at -18 dBFS about 11%. On a 1440p screen that is a small figure in a
//! large dark field, which is exactly what was reported live: "80% of the screen
//! full sized is just the panel's background".
//!
//! No amount of camera work fixes that -- it is arithmetic, not framing. Every
//! hardware vectorscope answers it the same way, with a gain control, and this
//! is that control.
//!
//! # Why it lives here
//!
//! Gain is a *display* decision, so it belongs to the host rather than to the
//! analysis: it never touches what the meters read, and turning it up does not
//! make a track louder or its correlation different. And it lives in this crate
//! rather than in either shell because the automatic mode has a state machine --
//! a smoothed peak with instant attack and a slow release -- and two copies of
//! that is two shells whose scopes visibly disagree about the same music. The
//! shells choose the setting; everything about what the setting *means* is here.

/// How much of the frame an automatically-gained signal is aimed at, as an
/// amplitude relative to full scale. Short of 1.0 so that a track which grows
/// louder than the last few seconds suggested has somewhere to grow into.
const AUTO_TARGET: f32 = 0.9;

/// How quickly automatic gain gives way as a passage gets louder: instantly.
/// The other direction is [`AUTO_RELEASE_SECONDS`].
///
/// Asymmetric on purpose, and this is the direction that matters -- a scope that
/// eased *into* a loud passage would clip its own display for as long as the
/// easing took, and the figure would be wrong rather than merely late.
const AUTO_RELEASE_SECONDS: f32 = 2.0;

/// The most an automatic gain will magnify. A near-silent passage would
/// otherwise be amplified until its noise floor filled the screen.
const AUTO_MAX: f32 = 16.0;

/// Below this the frame is silence rather than quiet audio, and gain waits at
/// unity: winding all the way up to [`AUTO_MAX`] between tracks would mean the
/// first frame of the next one arriving hugely magnified.
const SILENCE: f32 = 1e-4;

/// What the user chose. The fixed steps are the ones a scope traditionally
/// offers, in the units it offers them in -- a multiplier, not decibels,
/// because what it multiplies is a picture.
#[derive(Copy, Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum GainSetting {
    /// Draw amplitudes this much larger. 1.0 is unity: the figure's size is the
    /// signal's level, and full scale is the frame.
    Fixed(f32),
    /// Follow the music, aiming a recent peak at most of the frame. Honest but
    /// *not* a measurement -- the figure's size stops meaning level, which is
    /// why it is opt-in and why it is named where the UI can say so.
    Auto,
}

impl GainSetting {
    /// The steps the UI offers, in order. One registry, like `VisualKind`'s, so
    /// the panel, the commands and the smokes cannot offer different gains.
    pub const ALL: &'static [GainSetting] = &[
        GainSetting::Fixed(1.0),
        GainSetting::Fixed(2.0),
        GainSetting::Fixed(4.0),
        GainSetting::Fixed(8.0),
        GainSetting::Auto,
    ];

    /// Unity, and what the gallery opens at. A measurement by default: someone
    /// who wants the picture bigger can say so, where someone reading levels
    /// should never have to discover that the display was scaled.
    pub const DEFAULT: GainSetting = GainSetting::Fixed(1.0);

    /// The stable name, for a command argument or a stored setting.
    pub fn id(self) -> &'static str {
        match self {
            GainSetting::Fixed(1.0) => "1",
            GainSetting::Fixed(2.0) => "2",
            GainSetting::Fixed(4.0) => "4",
            GainSetting::Fixed(8.0) => "8",
            GainSetting::Fixed(_) => "custom",
            GainSetting::Auto => "auto",
        }
    }

    /// What a person reads on the button.
    pub fn label(self) -> &'static str {
        match self {
            GainSetting::Auto => "Auto",
            GainSetting::Fixed(1.0) => "x1",
            GainSetting::Fixed(2.0) => "x2",
            GainSetting::Fixed(4.0) => "x4",
            GainSetting::Fixed(8.0) => "x8",
            GainSetting::Fixed(_) => "custom",
        }
    }

    /// `None` for a name this build does not offer, so a stored setting from
    /// another version is a thing the caller decides about rather than something
    /// that silently rescales a measurement.
    pub fn from_id(id: &str) -> Option<Self> {
        GainSetting::ALL.iter().copied().find(|g| g.id() == id)
    }

    /// Whether the figure's size still means the signal's level.
    pub fn is_measurement(self) -> bool {
        matches!(self, GainSetting::Fixed(1.0))
    }
}

/// The setting plus what the automatic mode has learned. Held by the host, one
/// per renderer.
pub(crate) struct Gain {
    setting: GainSetting,
    /// The smoothed peak automatic gain is working from.
    level: f32,
}

impl Gain {
    pub(crate) fn new(setting: GainSetting) -> Self {
        Self {
            setting,
            level: 0.0,
        }
    }

    pub(crate) fn setting(&self) -> GainSetting {
        self.setting
    }

    pub(crate) fn set(&mut self, setting: GainSetting) {
        // The learned level is kept across a change rather than reset: switching
        // to Auto after a while of watching should settle immediately on what
        // the music has been doing, not wind up from silence.
        self.setting = setting;
    }

    /// The multiplier for this frame, advancing the automatic mode's state.
    pub(crate) fn resolve(&mut self, peak: f32, elapsed_seconds: f32) -> f32 {
        match self.setting {
            GainSetting::Fixed(gain) => {
                // Still followed while fixed, so that switching to Auto does not
                // start from whatever the level happened to be two tracks ago.
                self.follow(peak, elapsed_seconds);
                gain.max(0.0)
            }
            GainSetting::Auto => {
                let level = self.follow(peak, elapsed_seconds);
                if level <= SILENCE {
                    1.0
                } else {
                    (AUTO_TARGET / level).clamp(1.0, AUTO_MAX)
                }
            }
        }
    }

    /// Instant attack, exponential release. Returns the smoothed level.
    fn follow(&mut self, peak: f32, elapsed_seconds: f32) -> f32 {
        if peak >= self.level {
            self.level = peak;
        } else {
            // Framed in seconds rather than in frames so the release sounds --
            // looks -- the same on a 60Hz redraw thread and on a 144Hz browser.
            let decay = (-elapsed_seconds.max(0.0) / AUTO_RELEASE_SECONDS).exp();
            self.level = peak + (self.level - peak) * decay;
        }
        self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_setting_round_trips_through_its_id() {
        for setting in GainSetting::ALL {
            assert_eq!(GainSetting::from_id(setting.id()), Some(*setting));
        }
        assert_eq!(GainSetting::from_id("x2"), None, "the label is not the id");
        assert_eq!(GainSetting::from_id(""), None);
    }

    #[test]
    fn ids_and_labels_are_unique_and_the_default_is_offered() {
        let mut ids: Vec<_> = GainSetting::ALL.iter().map(|g| g.id()).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count);
        assert!(GainSetting::ALL.contains(&GainSetting::DEFAULT));
        // Only unity is a measurement -- the point of saying so in the UI.
        assert!(GainSetting::DEFAULT.is_measurement());
        assert!(!GainSetting::Auto.is_measurement());
        assert!(!GainSetting::Fixed(2.0).is_measurement());
    }

    #[test]
    fn a_fixed_gain_is_exactly_what_was_asked_for() {
        let mut gain = Gain::new(GainSetting::Fixed(4.0));
        // Whatever the audio does. A fixed gain that drifted would be the worst
        // of both modes: not a measurement, and not adaptive either.
        assert_eq!(gain.resolve(0.9, 1.0 / 60.0), 4.0);
        assert_eq!(gain.resolve(0.01, 1.0 / 60.0), 4.0);
        assert_eq!(gain.resolve(0.0, 1.0 / 60.0), 4.0);
    }

    #[test]
    fn auto_gain_lifts_a_quiet_passage_towards_the_frame() {
        let mut gain = Gain::new(GainSetting::Auto);
        // A track peaking at a quarter of full scale -- roughly -12 dBFS, which
        // is ordinary for a master and is the case that started all this.
        let resolved = gain.resolve(0.25, 1.0 / 60.0);
        assert!(
            (resolved - AUTO_TARGET / 0.25).abs() < 1e-4,
            "quarter scale resolved to {resolved}"
        );
        // And the point of it: what gets drawn now reaches most of the frame.
        assert!((0.25 * resolved - AUTO_TARGET).abs() < 1e-4);
    }

    #[test]
    fn auto_gain_never_shrinks_a_loud_one() {
        let mut gain = Gain::new(GainSetting::Auto);
        // Full scale already fills the frame; pulling it down would be
        // normalisation in the other direction, and would make a loud master
        // look like a quiet one.
        assert_eq!(gain.resolve(1.0, 1.0 / 60.0), 1.0);
        assert_eq!(gain.resolve(0.95, 1.0 / 60.0), 1.0);
    }

    #[test]
    fn auto_gain_gives_way_to_a_transient_at_once_and_recovers_slowly() {
        let mut gain = Gain::new(GainSetting::Auto);
        for _ in 0..200 {
            gain.resolve(0.05, 1.0 / 60.0);
        }
        let quiet = gain.resolve(0.05, 1.0 / 60.0);
        assert!(quiet > 4.0, "a quiet passage should be well magnified");

        // A snare hits full scale: the very next frame must already be at unity,
        // or the display clips for as long as the easing takes.
        assert_eq!(gain.resolve(1.0, 1.0 / 60.0), 1.0);

        // And it comes back gradually rather than snapping, so the picture does
        // not pump between hits. Half a second later it is still close to unity.
        let soon = gain.resolve(0.05, 0.5);
        assert!(soon < 1.5, "half a second later the gain was {soon}");
        // Several seconds of quiet, and it has opened up again.
        let later = gain.resolve(0.05, 6.0);
        assert!(later > 4.0, "six seconds later the gain was {later}");
    }

    #[test]
    fn silence_waits_at_unity_rather_than_winding_up() {
        let mut gain = Gain::new(GainSetting::Auto);
        for _ in 0..600 {
            assert_eq!(gain.resolve(0.0, 1.0 / 60.0), 1.0);
        }
        // So the first frame of the next track is drawn at its own level rather
        // than sixteen times it.
        assert!(gain.resolve(0.8, 1.0 / 60.0) <= 1.2);
    }

    #[test]
    fn the_release_is_measured_in_seconds_not_in_frames() {
        // The same elapsed time must give the same recovery whatever the frame
        // rate, or the two shells' scopes visibly disagree -- a 60Hz redraw
        // thread natively against whatever the browser feels like.
        let mut slow = Gain::new(GainSetting::Auto);
        let mut fast = Gain::new(GainSetting::Auto);
        slow.resolve(1.0, 1.0 / 60.0);
        fast.resolve(1.0, 1.0 / 60.0);
        for _ in 0..60 {
            slow.resolve(0.05, 1.0 / 60.0);
        }
        for _ in 0..144 {
            fast.resolve(0.05, 1.0 / 144.0);
        }
        let (a, b) = (
            slow.resolve(0.05, 1.0 / 60.0),
            fast.resolve(0.05, 1.0 / 144.0),
        );
        assert!((a - b).abs() < 0.05, "60Hz gave {a}, 144Hz gave {b}");
    }
}
