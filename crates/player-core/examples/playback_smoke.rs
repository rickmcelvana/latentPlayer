//! Scratch smoke test for the cpal output path -- run with:
//!   cargo run -p player-core --example playback_smoke -- <path>
//! Plays for ~1s, printing the scope window a couple of times so the ring
//! (well -- shared-cursor, see playback.rs's doc comment) is visibly
//! advancing without needing a human to actually listen.
use std::thread::sleep;
use std::time::Duration;

use player_core::{decode_all, LocalFile, Playback};

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: playback_smoke <path>");
    let audio = decode_all(&LocalFile::new(&path)).expect("decode failed");
    println!(
        "decoded {:.3}s, opening output device...",
        audio.duration_seconds()
    );

    let playback = Playback::open(audio).expect("failed to open output device");
    println!(
        "output device ready: {} ch @ {} Hz",
        playback.output_channels(),
        playback.output_sample_rate()
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
