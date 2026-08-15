//! Scratch smoke test for the decode pipeline -- not part of the crate's
//! public API or test suite, just a fast way to sanity-check decode_all()
//! against a real file without spinning up the whole Tauri app. Run with:
//!   cargo run -p player-core --example decode_smoke -- <path>
use player_core::{decode_all, LocalFile};

fn main() {
    let path = std::env::args().nth(1).expect("usage: decode_smoke <path>");
    let source = LocalFile::new(&path);
    let audio = decode_all(&source).expect("decode failed");
    println!(
        "decoded {} samples, {} channels, {} Hz, {:.3}s",
        audio.samples.len(),
        audio.channels,
        audio.sample_rate,
        audio.duration_seconds()
    );
    let nonzero = audio.samples.iter().filter(|s| s.abs() > 1e-6).count();
    println!("{nonzero} non-silent samples of {}", audio.samples.len());
}
