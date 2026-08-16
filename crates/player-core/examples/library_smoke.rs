//! Live check of the library scan, tag reading and index (Phase 1 M7).
//! Synthesises its own tagged files, so it needs no fixture and no arguments:
//!
//!   cargo run -p player-core --example library_smoke
//!
//! The native counterpart of `/library.html` in the web build, running the
//! same checks against the same claims so the two shells can be held to one
//! standard -- the habit M4, M5 and M6 all follow.
//!
//! Unlike its siblings this one opens no audio device: the library never
//! touches the audio path, which is itself one of M7's claims (decision 6).
//! What it does exercise is the whole of the real thing -- a real directory
//! tree, the real ID3v2 reader, real cover files written to a real cache --
//! rather than the unit tests' in-memory sources.

use std::fs;
use std::path::{Path, PathBuf};

use player_core::library::{Library, Playlist};

fn check(label: &str, ok: bool, detail: String) {
    println!("  [{}] {label}: {detail}", if ok { "PASS" } else { "FAIL" });
}

/// A silent 16-bit stereo WAV. The audio is irrelevant here -- what is being
/// scanned is the container and its tags -- so it is as short as it can be.
fn wav(frames: usize) -> Vec<u8> {
    let (channels, rate, bits) = (2u16, 44_100u32, 16u16);
    let block_align = channels * bits / 8;
    let data_len = frames * block_align as usize;

    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * block_align as u32).to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    out.resize(out.len() + data_len, 0);
    out
}

fn id3_frame(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(id);
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(body);
    out
}

fn text_frame(id: &[u8; 4], text: &str) -> Vec<u8> {
    let mut body = vec![0u8];
    body.extend_from_slice(text.as_bytes());
    id3_frame(id, &body)
}

fn picture_frame(image: &[u8]) -> Vec<u8> {
    let mut body = vec![0u8];
    body.extend_from_slice(b"image/jpeg");
    body.push(0);
    body.push(3); // front cover
    body.push(0); // empty description
    body.extend_from_slice(image);
    id3_frame(b"APIC", &body)
}

/// An ID3v2.3 tag. Sizes are syncsafe -- seven bits per byte.
fn id3v2(frames: &[Vec<u8>]) -> Vec<u8> {
    let body: Vec<u8> = frames.concat();
    let size = body.len() as u32;
    let mut out = Vec::new();
    out.extend_from_slice(b"ID3");
    out.extend_from_slice(&[3, 0, 0]);
    out.extend_from_slice(&[
        ((size >> 21) & 0x7f) as u8,
        ((size >> 14) & 0x7f) as u8,
        ((size >> 7) & 0x7f) as u8,
        (size & 0x7f) as u8,
    ]);
    out.extend_from_slice(&body);
    out
}

/// Writes a tagged file: an ID3v2 tag in front of a WAV. Symphonia's probe
/// reads leading metadata before it looks for a container, so this exercises
/// the real ID3v2 reader without synthesising an MP3.
fn write_track(path: &Path, frames: &[Vec<u8>]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut bytes = id3v2(frames);
    bytes.extend_from_slice(&wav(4_410));
    fs::write(path, bytes).unwrap();
}

fn scratch(name: &str) -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("latentplayer-smoke-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn main() {
    let music = scratch("music");
    let store = scratch("store");

    // One album of three, sharing a cover, plus a track by someone else and an
    // untagged file -- the four cases a library row has to render.
    let cover = {
        let mut bytes = vec![0xff, 0xd8, 0xff, 0xe0]; // JPEG signature
        bytes.extend_from_slice(&[0x42; 512]);
        bytes
    };
    for (n, title) in [(1, "Opening"), (2, "Phosphor"), (3, "Decay")] {
        write_track(
            &music.join(format!("Phosphor/{n:02} {title}.wav")),
            &[
                text_frame(b"TIT2", title),
                text_frame(b"TPE1", "The Vectorscopes"),
                text_frame(b"TALB", "Phosphor"),
                text_frame(b"TRCK", &format!("{n}/3")),
                text_frame(b"TYER", "1998"),
                text_frame(b"TCON", "Electronic"),
                picture_frame(&cover),
            ],
        );
    }
    write_track(
        &music.join("Singles/lone.wav"),
        &[
            text_frame(b"TIT2", "Lone Signal"),
            text_frame(b"TPE1", "Another Artist"),
        ],
    );
    // No tag at all, and two files the scan must ignore.
    fs::write(music.join("Singles/untagged.wav"), wav(4_410)).unwrap();
    fs::write(music.join("Singles/notes.txt"), b"not music").unwrap();
    fs::write(music.join("folder.jpg"), [0u8; 16]).unwrap();

    println!("library smoke -- scanning {}", music.display());

    let mut library = Library::open(&store).unwrap();
    let summary = library.scan(&music).unwrap();

    check(
        "the scan finds audio files and only audio files",
        summary.found == 5 && summary.added == 5 && summary.failed == 0,
        format!(
            "found {}, added {}, failed {} (a .txt and a .jpg were present)",
            summary.found, summary.added, summary.failed
        ),
    );

    // Cloned rather than borrowed: the rescan check below needs the library
    // back, and these rows are compared against what it holds afterwards.
    let phosphor: Vec<_> = library
        .tracks()
        .iter()
        .filter(|t| t.album.as_deref() == Some("Phosphor"))
        .cloned()
        .collect();
    check(
        "tags come back off the real ID3v2 reader",
        phosphor.len() == 3
            && phosphor.iter().all(|t| {
                t.artist.as_deref() == Some("The Vectorscopes")
                    && t.year == Some(1998)
                    && t.genre.as_deref() == Some("Electronic")
            }),
        format!(
            "{} tracks on the album, first is {:?} by {:?} ({:?})",
            phosphor.len(),
            phosphor.first().and_then(|t| t.title.clone()),
            phosphor.first().and_then(|t| t.artist.clone()),
            phosphor.first().and_then(|t| t.year),
        ),
    );

    let numbers: Vec<Option<u32>> = phosphor.iter().map(|t| t.track_number).collect();
    check(
        "track numbers survive the \"7/12\" spelling",
        numbers == [Some(1), Some(2), Some(3)],
        format!("{numbers:?}"),
    );

    // The point of hashing the image rather than the track: an album's tracks
    // must name one cached file between them, not one each.
    let cover_ids: Vec<_> = phosphor.iter().filter_map(|t| t.cover_id.clone()).collect();
    let deduped = cover_ids
        .first()
        .is_some_and(|id| cover_ids.iter().all(|c| c == id));
    check(
        "one album's cover is cached once, not once per track",
        cover_ids.len() == 3 && deduped,
        format!("{} covers, {} distinct", cover_ids.len(), {
            let mut unique = cover_ids.clone();
            unique.sort();
            unique.dedup();
            unique.len()
        }),
    );

    let cached = cover_ids
        .first()
        .map(|id| library.cover_path(id))
        .filter(|p| p.exists())
        .and_then(|p| fs::read(p).ok());
    check(
        "the cached cover is the bytes that were embedded",
        cached.as_deref() == Some(cover.as_slice()),
        match &cached {
            Some(bytes) => format!("{} bytes on disk, {} embedded", bytes.len(), cover.len()),
            None => "no cover file was written".to_string(),
        },
    );

    let untagged = library
        .tracks()
        .iter()
        .find(|t| t.file_name == "untagged.wav")
        .expect("the untagged file is still a row");
    check(
        "an untagged file is still a usable row",
        untagged.title.is_none()
            && untagged.sample_rate == Some(44_100)
            && untagged.duration_seconds.is_some(),
        format!(
            "title {:?}, {:?}Hz, {:.3}s -- the UI falls back to \"{}\"",
            untagged.title,
            untagged.sample_rate,
            untagged.duration_seconds.unwrap_or(0.0),
            untagged.file_name
        ),
    );

    // A rescan must be idempotent. The failure this catches is the obvious
    // one -- scan twice, get a library twice the size.
    let first_ids: Vec<String> = library.tracks().iter().map(|t| t.id.clone()).collect();
    library.scan(&music).unwrap();
    let second_ids: Vec<String> = library.tracks().iter().map(|t| t.id.clone()).collect();
    check(
        "rescanning updates rows rather than duplicating them",
        first_ids == second_ids,
        format!(
            "{} rows before, {} after",
            first_ids.len(),
            second_ids.len()
        ),
    );

    library
        .save_playlist(Playlist {
            id: "p1".into(),
            name: "Evening".into(),
            track_ids: phosphor.iter().map(|t| t.id.clone()).collect(),
        })
        .unwrap();

    // Reopening is the whole point of persisting: a library that has to be
    // rescanned on every launch is not a library.
    drop(library);
    let reopened = Library::open(&store).unwrap();
    check(
        "the index survives a restart",
        reopened.tracks().len() == 5 && reopened.folders().len() == 1,
        format!(
            "{} rows and {} folder(s) read back from disk",
            reopened.tracks().len(),
            reopened.folders().len()
        ),
    );

    let playlist = reopened.playlists().first();
    let refs_resolve = playlist.is_some_and(|p| {
        p.track_ids
            .iter()
            .all(|id| reopened.tracks().iter().any(|t| &t.id == id))
    });
    check(
        "a saved playlist survives and its references still resolve",
        playlist.is_some_and(|p| p.name == "Evening" && p.track_ids.len() == 3) && refs_resolve,
        match playlist {
            Some(p) => format!(
                "\"{}\" with {} tracks, all still in the library",
                p.name,
                p.track_ids.len()
            ),
            None => "no playlist was read back".to_string(),
        },
    );

    let _ = fs::remove_dir_all(&music);
    let _ = fs::remove_dir_all(&store);
}
