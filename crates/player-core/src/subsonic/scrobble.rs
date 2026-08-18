//! Scrobbling (Phase 2 M4): telling the server what is playing, and what was
//! played.
//!
//! Two things, sent to one endpoint. `submission=false` is "now playing" -- a
//! notification the server holds for a while and shows to whoever is looking.
//! `submission=true` is a **play**, which increments a count and, on a server
//! configured for it, is relayed onward to Last.fm or ListenBrainz. The second
//! is the one worth being careful about: it writes to a history this app cannot
//! edit, in a place the listener may have years of.
//!
//! # When a play counts
//!
//! Last.fm's own rule, adopted rather than invented (decision 9 in
//! tasks/phase-2.md): a track longer than 30 seconds submits once the listener
//! has heard **half of it, or four minutes of it, whichever comes first**. The
//! reason to adopt rather than invent is where the scrobble ends up -- a client
//! with its own threshold either double-counts against services that already
//! agreed on this one, or drops plays they would have kept.
//!
//! "Heard" is time *played*, not the position reached. Seeking to the last
//! thirty seconds of a track is not listening to it, and a threshold measured
//! off the position alone would count it as a play. [`ScrobbleTracker`]
//! therefore accumulates position deltas only while they look like continuous
//! playback -- a forward delta no larger than the wall-clock interval that
//! produced it -- so a seek advances the position and adds nothing.
//!
//! # What this module still does not do
//!
//! Any IO, and any reading of the clock. The shell passes the time in, exactly
//! as it passes randomness in for the auth salt (decision 3), so this stays a
//! module both a native thread and a wasm binding can hold -- which is the whole
//! point of the rule living here rather than twice in TypeScript (decision 10).

/// Shortest track that may be submitted at all. Below this the rule does not
/// apply and nothing is ever counted -- an interlude played on a loop is not a
/// hundred plays.
pub const MIN_TRACK_SECONDS: f64 = 30.0;

/// Most listening the rule ever asks for, whatever the track's length.
pub const MAX_REQUIRED_SECONDS: f64 = 240.0;

/// How much further than the wall-clock interval a position may advance and
/// still count as continuous playback.
///
/// Not zero, because neither shell's poll is exact: the position is read from a
/// device that has moved on since, and the interval is measured around a
/// round trip. Half a second is far above that jitter and far below any seek a
/// person makes on purpose.
const CONTINUITY_TOLERANCE_SECONDS: f64 = 0.5;

/// The parameters for one scrobble.
///
/// `time` is when the play *started*, in milliseconds since the epoch -- not
/// when it was submitted. A client that sends the submission time puts every
/// track in the listener's history four minutes late, and for a long album that
/// error accumulates.
pub fn scrobble_params(id: &str, submission: bool, time_ms: Option<u64>) -> Vec<(String, String)> {
    let mut params = vec![
        ("id".to_string(), id.to_string()),
        (
            "submission".to_string(),
            if submission { "true" } else { "false" }.to_string(),
        ),
    ];
    if let Some(time) = time_ms {
        params.push(("time".to_string(), time.to_string()));
    }
    params
}

/// One scrobble, as a request both shells can send across their own boundary --
/// the same shape `BrowseRequest` and `PlaylistRequest` take, so a browser
/// building this URL still names no endpoint and no parameter.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrobbleRequest {
    pub id: String,
    /// `true` is a play, `false` is "now playing" -- see this module's docs for
    /// which one writes to a history.
    pub submission: bool,
    /// When the play *started*, milliseconds since the epoch.
    #[serde(default)]
    pub time_ms: Option<u64>,
}

impl ScrobbleRequest {
    pub fn endpoint(&self) -> &'static str {
        "scrobble"
    }

    pub fn params(&self) -> Vec<(String, String)> {
        scrobble_params(&self.id, self.submission, self.time_ms)
    }
}

/// What the tracker has decided should be sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScrobbleAction {
    /// `submission=false`: this track has started playing.
    NowPlaying { id: String },
    /// `submission=true`: this track has been listened to. `time_ms` is when the
    /// play began.
    Submit { id: String, time_ms: u64 },
}

/// One track being listened to, and how much of it has actually been heard.
#[derive(Debug, Clone)]
struct Listening {
    id: String,
    /// Seconds of continuous playback accumulated so far.
    heard: f64,
    /// Where the position was, and when, at the last observation.
    last_position: f64,
    last_now_ms: u64,
    /// When this play began, which is what a submission reports as its `time`.
    started_ms: u64,
    now_playing_sent: bool,
    submitted: bool,
}

/// Decides what to scrobble, from the position the shell is already polling.
///
/// Driven by [`ScrobbleTracker::observe`] on both shells and holding no clock,
/// no network and no opinion about which one it is running on. One deliberate
/// simplification: a play is submitted at most once per run of a track id, so
/// restarting the track that just finished -- or seeking back to its start --
/// does not earn a second play. Distinguishing "played it again" from "seeked
/// backwards" is not something the position can answer, and the direction to err
/// in is the one that does not write to someone's history twice.
#[derive(Debug, Clone, Default)]
pub struct ScrobbleTracker {
    current: Option<Listening>,
}

impl ScrobbleTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Tells the tracker where playback is, and gets back what to send.
    ///
    /// - `track_id` is the server's id for what is playing, or `None` when what
    ///   is playing is not a server track (a local file scrobbles nowhere in
    ///   this milestone -- see PLAN.md §7 for where direct Last.fm goes).
    /// - `duration_seconds` may be 0, which means the server did not state one;
    ///   the rule falls back to four minutes of listening (decision 9).
    /// - `now_ms` is milliseconds since the epoch, from the shell's own clock.
    ///
    /// Returns zero, one or two actions -- two when a track short enough to
    /// cross the threshold between polls starts and submits in the same tick,
    /// which a 30-second track at half speed cannot but a test can.
    pub fn observe(
        &mut self,
        track_id: Option<&str>,
        position_seconds: f64,
        duration_seconds: f64,
        playing: bool,
        now_ms: u64,
    ) -> Vec<ScrobbleAction> {
        let Some(id) = track_id else {
            self.current = None;
            return Vec::new();
        };

        // A different track: whatever the last one had accumulated is gone, and
        // the new one starts from nothing.
        if self.current.as_ref().is_none_or(|played| played.id != id) {
            self.current = Some(Listening {
                id: id.to_string(),
                heard: 0.0,
                last_position: position_seconds,
                last_now_ms: now_ms,
                started_ms: now_ms,
                now_playing_sent: false,
                submitted: false,
            });
        }

        let Some(played) = self.current.as_mut() else {
            return Vec::new();
        };

        let mut actions = Vec::new();

        if !playing {
            // A paused track accumulates nothing and announces nothing, but the
            // clock still has to move or the pause would be credited as
            // listening the moment it resumes.
            played.last_position = position_seconds;
            played.last_now_ms = now_ms;
            return actions;
        }

        if !played.now_playing_sent {
            played.now_playing_sent = true;
            actions.push(ScrobbleAction::NowPlaying {
                id: played.id.clone(),
            });
        }

        // The continuity test: how far the position moved, against how much time
        // passed while it moved. A seek fails it in both directions -- backwards
        // is negative, forwards is larger than the interval -- and adds nothing.
        let wall_seconds = now_ms.saturating_sub(played.last_now_ms) as f64 / 1000.0;
        let advanced = position_seconds - played.last_position;
        if advanced >= 0.0 && advanced <= wall_seconds + CONTINUITY_TOLERANCE_SECONDS {
            played.heard += advanced;
        }
        played.last_position = position_seconds;
        played.last_now_ms = now_ms;

        if !played.submitted && earned(played.heard, duration_seconds) {
            played.submitted = true;
            actions.push(ScrobbleAction::Submit {
                id: played.id.clone(),
                time_ms: played.started_ms,
            });
        }

        actions
    }

    /// Forgets what is playing, so the next observation starts a fresh play.
    pub fn reset(&mut self) {
        self.current = None;
    }

    /// Seconds of continuous playback accumulated for what is playing. For
    /// tests and for a shell that wants to show the rule working.
    pub fn heard_seconds(&self) -> f64 {
        self.current.as_ref().map_or(0.0, |played| played.heard)
    }
}

/// The rule itself, in one place: has `heard` seconds of a `duration`-second
/// track earned a play?
///
/// A duration of 0 means the server did not state one, which is a case this
/// phase already knows about. The "half of it" half of the rule needs a
/// duration; the four-minute half does not, so that is what applies.
pub fn earned(heard_seconds: f64, duration_seconds: f64) -> bool {
    if duration_seconds > 0.0 && duration_seconds < MIN_TRACK_SECONDS {
        return false;
    }
    let required = if duration_seconds > 0.0 {
        (duration_seconds / 2.0).min(MAX_REQUIRED_SECONDS)
    } else {
        MAX_REQUIRED_SECONDS
    };
    heard_seconds >= required
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_playing_and_a_submission_are_the_same_endpoint_said_differently() {
        let now = scrobble_params("s1", false, None);
        assert_eq!(now[0], ("id".to_string(), "s1".to_string()));
        assert_eq!(now[1], ("submission".to_string(), "false".to_string()));
        assert_eq!(now.len(), 2, "now-playing carries no time");

        let submit = scrobble_params("s1", true, Some(1_700_000_000_000));
        assert_eq!(submit[1], ("submission".to_string(), "true".to_string()));
        assert_eq!(submit[2], ("time".to_string(), "1700000000000".to_string()));
    }

    #[test]
    fn the_rule_is_half_the_track_or_four_minutes() {
        // A three-minute track: half of it.
        assert!(!earned(89.0, 180.0));
        assert!(earned(90.0, 180.0));
        // A twenty-minute track: four minutes, not ten.
        assert!(!earned(239.0, 1200.0));
        assert!(earned(240.0, 1200.0));
    }

    #[test]
    fn a_track_under_thirty_seconds_never_counts() {
        assert!(!earned(29.0, 29.0));
        assert!(!earned(1_000.0, 20.0));
        // Exactly thirty does, which is the boundary the rule states.
        assert!(earned(15.0, 30.0));
    }

    #[test]
    fn an_unknown_duration_waits_for_four_minutes() {
        assert!(!earned(239.0, 0.0));
        assert!(earned(240.0, 0.0));
    }

    /// The behaviour the whole state machine exists for: a seek is not
    /// listening.
    #[test]
    fn seeking_to_the_end_does_not_earn_a_play() {
        let mut tracker = ScrobbleTracker::new();
        // Two seconds of real playback...
        tracker.observe(Some("s1"), 0.0, 300.0, true, 0);
        tracker.observe(Some("s1"), 1.0, 300.0, true, 1_000);
        tracker.observe(Some("s1"), 2.0, 300.0, true, 2_000);
        // ...then a jump to 4:59, which is a click and not four minutes of
        // listening.
        let actions = tracker.observe(Some("s1"), 299.0, 300.0, true, 2_100);
        assert!(actions.is_empty(), "a seek must not submit: {actions:?}");
        assert!((tracker.heard_seconds() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn playing_through_earns_exactly_one_play() {
        let mut tracker = ScrobbleTracker::new();
        let first = tracker.observe(Some("s1"), 0.0, 60.0, true, 0);
        assert_eq!(
            first,
            vec![ScrobbleAction::NowPlaying { id: "s1".into() }],
            "the start of a track announces itself"
        );

        let mut submissions = 0;
        // Poll once a second for the whole minute, as the app does (faster).
        for second in 1..=60u64 {
            let actions = tracker.observe(Some("s1"), second as f64, 60.0, true, second * 1_000);
            for action in actions {
                match action {
                    ScrobbleAction::Submit { id, time_ms } => {
                        assert_eq!(id, "s1");
                        assert_eq!(time_ms, 0, "the play is timed from when it started");
                        // Half of sixty seconds.
                        assert_eq!(second, 30);
                        submissions += 1;
                    }
                    ScrobbleAction::NowPlaying { .. } => {
                        panic!("now-playing must be sent once, not per poll")
                    }
                }
            }
        }
        assert_eq!(submissions, 1);
    }

    #[test]
    fn a_pause_is_not_listening_and_does_not_resume_as_if_it_were() {
        let mut tracker = ScrobbleTracker::new();
        tracker.observe(Some("s1"), 0.0, 100.0, true, 0);
        tracker.observe(Some("s1"), 10.0, 100.0, true, 10_000);
        // Paused for an hour at the same position.
        tracker.observe(Some("s1"), 10.0, 100.0, false, 3_610_000);
        // Resuming credits the hour to nothing.
        let actions = tracker.observe(Some("s1"), 11.0, 100.0, true, 3_611_000);
        assert!(actions.is_empty(), "no second now-playing, no submission");
        assert!((tracker.heard_seconds() - 11.0).abs() < 1e-9);
    }

    #[test]
    fn a_new_track_starts_over() {
        let mut tracker = ScrobbleTracker::new();
        tracker.observe(Some("s1"), 0.0, 100.0, true, 0);
        tracker.observe(Some("s1"), 40.0, 100.0, true, 40_000);
        let actions = tracker.observe(Some("s2"), 0.0, 100.0, true, 40_100);
        assert_eq!(
            actions,
            vec![ScrobbleAction::NowPlaying { id: "s2".into() }]
        );
        assert_eq!(tracker.heard_seconds(), 0.0);
    }

    #[test]
    fn a_local_track_scrobbles_nothing_and_clears_what_was_playing() {
        let mut tracker = ScrobbleTracker::new();
        tracker.observe(Some("s1"), 0.0, 100.0, true, 0);
        tracker.observe(Some("s1"), 10.0, 100.0, true, 10_000);
        assert!(tracker.observe(None, 0.0, 0.0, true, 10_100).is_empty());
        assert_eq!(tracker.heard_seconds(), 0.0);
        // And the same track afterwards is a fresh play, announced again.
        let actions = tracker.observe(Some("s1"), 0.0, 100.0, true, 20_000);
        assert_eq!(
            actions,
            vec![ScrobbleAction::NowPlaying { id: "s1".into() }]
        );
    }

    #[test]
    fn resetting_forgets_what_was_playing() {
        let mut tracker = ScrobbleTracker::new();
        tracker.observe(Some("s1"), 0.0, 100.0, true, 0);
        tracker.observe(Some("s1"), 30.0, 100.0, true, 30_000);
        tracker.reset();
        assert_eq!(tracker.heard_seconds(), 0.0);
    }

    /// The poll is not exact -- the position is read from a device that has
    /// moved on -- so a tick that reports slightly more than the interval is
    /// still playback, not a seek.
    #[test]
    fn small_jitter_still_counts_as_listening() {
        let mut tracker = ScrobbleTracker::new();
        tracker.observe(Some("s1"), 0.0, 100.0, true, 0);
        tracker.observe(Some("s1"), 1.3, 100.0, true, 1_000);
        assert!((tracker.heard_seconds() - 1.3).abs() < 1e-9);
    }
}
