//! Scratch smoke test for the cpal output path -- run with:
//!   cargo run -p player-core --example playback_smoke -- <path>
//! Plays for ~1s, printing the scope window a couple of times so the ring is
//! visibly advancing without needing a human to actually listen.
use std::thread::sleep;
use std::time::Duration;

use player_core::{LocalFile, Playback};

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: playback_smoke <path>");

    let playback =
        Playback::open(Box::new(LocalFile::new(&path))).expect("failed to open output device");
    println!(
        "output device ready: {} ch @ {} Hz, track is {:.3}s",
        playback.output_channels(),
        playback.output_sample_rate(),
        playback.duration_seconds()
    );
    playback.play();

    for i in 0..3 {
        sleep(Duration::from_millis(300));
        let window = playback.scope_window(64);
        println!(
            "t={:.1}s playing={} scope_window first samples: {:?}",
            i as f32 * 0.3,
            playback.is_playing(),
            &window[..window.len().min(8)]
        );
    }
}
