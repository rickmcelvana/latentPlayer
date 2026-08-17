//! Live check of remote playback against a **real** server (Phase 2 M2).
//!
//!   cargo run -p player-core --example remote_smoke
//!
//! `cargo test -p player-core --test remote_source` covers what a fake server
//! can be made to do, including the check that matters most -- audio over HTTP
//! decoding identically to the same bytes locally. This covers what it cannot: a
//! real Navidrome, a real track off a real disk, and a real output device.
//!
//! Needs credentials, through the environment for `subsonic_smoke`'s reason (a
//! password on a command line is in the shell history and in every process
//! listing):
//!
//! ```text
//!   LATENTPLAYER_SUBSONIC_URL       (default http://localhost:4533)
//!   LATENTPLAYER_SUBSONIC_USER
//!   LATENTPLAYER_SUBSONIC_PASSWORD
//! ```
//!
//! Plays real sound on the real default device, quietly.

use std::thread::sleep;
use std::time::{Duration, Instant};

use player_core::subsonic::http::HttpClient;
use player_core::subsonic::{Credentials, Server};
use player_core::Playback;

const DEFAULT_URL: &str = "http://localhost:4533";

fn check(label: &str, ok: bool, detail: String) {
    println!("  [{}] {label}: {detail}", if ok { "PASS" } else { "FAIL" });
}

fn near(got: f64, want: f64, tol: f64) -> bool {
    (got - want).abs() <= tol
}

fn main() {
    let url = std::env::var("LATENTPLAYER_SUBSONIC_URL").unwrap_or_else(|_| DEFAULT_URL.into());
    let (Ok(user), Ok(password)) = (
        std::env::var("LATENTPLAYER_SUBSONIC_USER"),
        std::env::var("LATENTPLAYER_SUBSONIC_PASSWORD"),
    ) else {
        println!(
            "  [SKIP] everything: set LATENTPLAYER_SUBSONIC_USER and \
             LATENTPLAYER_SUBSONIC_PASSWORD (and optionally _URL) to run this"
        );
        return;
    };

    let server = match Server::new(&url) {
        Ok(server) => server,
        Err(e) => {
            println!("{url} is not a usable server address: {e}");
            return;
        }
    };
    let client = HttpClient::new(server, Credentials::Password { user, password });

    // --- find something to play -------------------------------------------
    //
    // Random rather than a fixed id, deliberately: a check that only ever plays
    // one track proves that one track works. Browsing properly is M3.
    let songs = match client.random_songs(3) {
        Ok(songs) if !songs.is_empty() => songs,
        Ok(_) => {
            println!("  [SKIP] everything: the server has no songs in it yet");
            return;
        }
        Err(e) => {
            println!("  [FAIL] asking the server for something to play: {e}");
            return;
        }
    };
    let song = &songs[0];
    println!(
        "track:   {} -- {} ({}, {}s, {})",
        song.artist,
        song.title,
        song.album,
        song.duration.unwrap_or(0),
        song.suffix
    );

    // --- open it, which is the first network read -------------------------
    let started = Instant::now();
    let source = match client.track_source(&song.id, &song.suffix) {
        Ok(source) => source,
        Err(e) => {
            println!("  [FAIL] building a stream URL: {e}");
            return;
        }
    };
    let playback = match Playback::open(Box::new(source)) {
        Ok(playback) => playback,
        Err(e) => {
            println!("  [FAIL] opening the track over the network: {e}");
            return;
        }
    };
    let opened = started.elapsed();
    playback.set_volume(0.1); // be kind to whoever is at the keyboard

    check(
        "a server track opens over the network",
        true,
        format!(
            "{:.0}ms to first audio, {} ch @ {} Hz out",
            opened.as_secs_f64() * 1000.0,
            playback.output_channels(),
            playback.output_sample_rate()
        ),
    );

    // The server states a duration in the browse response; the container states
    // one too. They should agree, and disagreeing means the wrong track was
    // opened -- which a URL built with the wrong id would do silently.
    let stated = song.duration.unwrap_or(0) as f64;
    let container = playback.duration_seconds();
    check(
        "the track opened is the track asked for",
        stated == 0.0 || container == 0.0 || near(container, stated, 2.0),
        format!("the server says {stated:.0}s, the container says {container:.1}s"),
    );

    // --- play it ----------------------------------------------------------
    playback.play();
    sleep(Duration::from_millis(1500));
    let after = playback.position_seconds();
    check(
        "playback advances in real time over the network",
        near(after, 1.5, 0.35),
        format!("{after:.3}s after 1.5s of wall clock"),
    );

    // Audio actually reaching the device, rather than a position counter moving
    // on its own: the tap holds what was played, and silence would read as
    // zeroes. A track that is genuinely quiet here would be a false negative,
    // which is why this asks for *any* non-zero sample rather than a level.
    let window = playback.scope_window(4096);
    let peak = window.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
    check(
        "real audio reached the output",
        peak > 0.0,
        format!("peak {peak:.4} over {} samples of history", window.len()),
    );

    // --- seek, which is where a range request has to be right -------------
    let target = (container * 0.6).max(3.0);
    playback.seek(target);
    sleep(Duration::from_millis(800));
    let after_seek = playback.position_seconds();
    check(
        "seeking a network track lands and keeps playing",
        near(after_seek, target + 0.8, 0.4),
        format!("{after_seek:.3}s after seeking to {target:.1}s and waiting 0.8s"),
    );

    // --- and the gapless hand-off, over the network -----------------------
    if songs.len() > 1 {
        let next = &songs[1];
        match client.track_source(&next.id, &next.suffix) {
            Ok(source) => {
                playback.set_next(Box::new(source));
                // Straight to the end of the current track, so the seam arrives
                // in a second rather than in three minutes.
                let near_end = (container - 1.0).max(0.0);
                playback.seek(near_end);
                let mut handed_over = false;
                for _ in 0..60 {
                    sleep(Duration::from_millis(100));
                    if playback.track_index() > 0 {
                        handed_over = true;
                        break;
                    }
                }
                check(
                    "the queued server track takes over at the seam",
                    handed_over,
                    format!(
                        "track_index={} after the first track ended (next: {})",
                        playback.track_index(),
                        next.title
                    ),
                );
            }
            Err(e) => check(
                "the queued server track takes over at the seam",
                false,
                format!("{e}"),
            ),
        }
    }

    drop(playback);
}
