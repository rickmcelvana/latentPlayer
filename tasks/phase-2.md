# Phase 2 -- Self-hosted playback

PLAN.md §7's Phase 2: "Subsonic/Navidrome/Jellyfin support -- zero legal risk,
immediately unlocks large existing libraries for self-hosting users."

Phase 1 is closed (`tasks/phase-1.md`): both shells are a real local player --
streaming decode, queue and gapless, library and tags, playlists, EQ, the full
metering suite and the 3D vectorscope, all user-confirmed, with both of its
closing bugs fixed. What this phase adds is a second *source* of audio and
nothing else: the engine already takes a `PlaybackSource` and cannot tell where
the bytes came from, which is the whole reason a network library is a phase
rather than a rewrite. Every meter, the scope, the EQ and the transport work
unchanged the moment the bytes arrive -- and "point the LUFS meter at my own
server's library" is closer to PLAN.md §1's actual pitch than local files were.

## What already exists to build on

- `PlaybackSource` (`source.rs`) hands the decoder a Symphonia `MediaSource`:
  blocking `Read + Seek`, plus `is_seekable`/`byte_len`. `LocalFile` and
  `InMemorySource` are the two implementations; a remote one is the third, and
  the trait was written for it (its own doc comment says so).
- `StreamingDecoder` + the decoder thread already pull a block at a time and
  are already *allowed to block* -- they are not the audio callback. A network
  read belongs there and nowhere else.
- `tags.rs`, `library.rs`, the `cover://` scheme, `libraryStore.ts` and
  `LibraryPanel` are M7's, and a server's rows have to reach the same UI.
- The web build's audio path (`worker.ts` -> SAB ring -> worklet) already
  starts from a `Uint8Array` of encoded bytes handed to `WasmDecoder`. A track
  fetched over HTTP is the same shape as a track from the file picker.

## Decisions for this phase (2026-08-17)

1. **Subsonic/OpenSubsonic only. Jellyfin is not a Phase 2 target.** PLAN.md §7
   names Jellyfin alongside Navidrome, and that turns out to be wrong on the
   facts: Jellyfin has no native Subsonic API, only a third-party plugin whose
   own README calls it experimental. Its REST API is a separate surface --
   different auth, different browse model, different stream URLs -- i.e. a
   second client, not a second base URL. One protocol here covers Navidrome,
   Airsonic, Gonic, Ampache and Subsonic itself, and covers Jellyfin for anyone
   running the plugin. PLAN.md §3/§7 are corrected to say so rather than left
   to mislead the next reader; a native Jellyfin backend, if it is ever wanted,
   goes behind the same seam in a later phase.
2. **The protocol lives in Rust; the transport does not.** `player-core` builds
   the requests (URL, auth parameters, versioning) and parses the responses
   (including Subsonic's error envelope) for both shells. What it does *not*
   contain is a single line of IO: the desktop shell performs the request with
   a blocking HTTP client, and the web shell performs it with `fetch` in the
   worker and hands the bytes back to Rust to parse. This is M8's split applied
   to a network protocol instead of to a filter -- one definition of what a
   request means and what a response says, two ways of moving the bytes -- and
   it is what stops the two shells disagreeing about, say, an error code.
3. **Randomness is the shell's, hashing is the core's.** Token auth needs a
   fresh salt per request; `getrandom` on `wasm32-unknown-unknown` needs its own
   feature flag and a build-time opinion. So each transport generates a salt
   from its own platform RNG (`getrandom` natively, `crypto.getRandomValues` on
   the web) and passes it in. The core owns the hash, and owns it against the
   spec's published worked example: password `sesame` + salt `c19b2d` must give
   `26719a1196d2a940705a59634eb18eab`. A known-answer test, not a
   self-consistent one.
4. **API key where the server offers it, password in the OS keychain where it
   does not.** Subsonic's token scheme hashes the password *per request*, so a
   client that supports it must keep the password itself -- there is no
   derived-secret-at-rest option. OpenSubsonic's API-key extension removes that,
   so the client asks (`getOpenSubsonicExtensions`) and prefers a key when one
   is available. Otherwise: the OS credential store natively, IndexedDB on the
   web -- where the guarantee is weaker, and the UI says so in a sentence rather
   than leaving the user to assume parity.
5. **Web parity stays lockstep, and the limits are stated rather than hidden.**
   Phase 1's rule survives: each milestone lands on both shells before the next
   starts. Two limits are real and neither is a bug to fix -- a browser can only
   reach a server that sends CORS headers (Navidrome does), and a page served
   over HTTPS cannot talk to an `http://` server at all. The web build has to
   *diagnose* both by name, because "it doesn't work" against a server that
   plainly works in the desktop build is the worst possible failure message.
6. **Browse the server live; do not mirror its index.** The server already has a
   library and a search endpoint -- Navidrome's is faster than anything this app
   would build over a copy of it, and a mirror means a sync design, a staleness
   window and a migration for the on-disk index. Rows are fetched per view and
   cached for the session only. What this gives up is one search box over local
   and server music at once; that is a real feature and it is deferred rather
   than pretended away (see "Deferred").
7. **Ask for the original file, not a transcode.** The decoder here is
   Symphonia, on the user's own machine, so a server-side transcode is wasted
   work at best -- and at worst it is *unseekable*: a transcoding response is
   typically chunked, with no `Content-Length` and no `Accept-Ranges`, which is
   exactly the two things `MediaSource::byte_len` and a seek need. So: request
   the raw stream, verify the response can be ranged, and fall back to buffering
   the whole track when it cannot. The fallback is not hypothetical; it is what
   the web shell does for every track in M2 anyway.
8. **Nothing in this phase touches the audio thread, the DSP path or the
   renderer.** If a milestone here finds itself editing `playback.rs`'s callback,
   `eq.rs` or `player-render`, that is the signal that something has been
   designed wrong. The one exception this phase should expect is *buffering
   policy* -- a two-second ring was sized against a local disk (see M2).

## Milestones

Same rule as Phase 1: a milestone is done only when it works on **both** shells
and has been verified live. Compiling clean is not evidence, and against a
network protocol it is barely a hint.

- **M1 -- Connection and credentials.** `subsonic.rs` in `player-core`: request
  building, the common parameters (`u`/`t`/`s`/`v`/`c`/`f=json`), API-key auth,
  response parsing, and Subsonic's error envelope -- which returns **HTTP 200
  with `"status": "failed"`** and a code (40 = wrong credentials, 70 = not
  found), so a client that only checks the HTTP status believes every failure
  succeeded. Plus `ping`, `getOpenSubsonicExtensions`, credential storage per
  decision 4, and a settings UI on both shells for adding a server and seeing
  why it will not connect.
- **M2 -- Remote playback.** The phase's actual point. Natively a
  `RemoteSource` implementing `PlaybackSource` over HTTP range requests with
  read-ahead; on the web, the worker fetches the track's bytes and feeds the
  `InMemorySource` path it already uses for picked files. Then everything the
  transport already does has to keep working over it: seek, queue, gapless
  hand-off (which now means *prefetching* the next track before the seam), and
  underrun behaviour when the network stalls rather than the disk.
- **M3 -- Browsing the server.** `getArtists`/`getAlbumList2`/`getAlbum`/
  `getSong`/`search3`, and cover art through `getCoverArt` -- served to the
  webview through the existing `cover://` scheme natively, and as blob URLs on
  the web, because a cross-origin `<img src>` is a no-cors load and this app's
  own COEP header blocks those. The library panel grows a source switcher; the
  rows, the search box and "play this now" stay one implementation.
- **M4 -- Playlists, scrobbling, stars and ratings.** Server playlists beside
  the local ones (`getPlaylists`/`createPlaylist`/`updatePlaylist`), `scrobble`
  in both its forms -- `submission=false` is "now playing", `submission=true` is
  the play count, and the threshold for the latter is a decision this milestone
  has to make and write down -- and `star`/`unstar`/`setRating` on the rows.
- **M5 -- Failing well.** Timeouts, a server that goes away mid-track, a token
  the server has stopped accepting, a self-signed certificate (common on
  self-hosted boxes, and rejected by default by any modern TLS stack), and what
  the UI says in each case. Deliberately last, and deliberately *not* where all
  error handling goes: each milestone above handles its own failures, and this
  one is the pass that hunts for the ones nobody thought of.

Ordering rationale: M1 is the seam every other milestone calls through, M2 is
the feature the phase exists for and the riskiest engineering in it, M3-M4 are
what make it usable, M5 is the hardening pass a network feature earns.

## What "verified" has to mean here

Phase 1's checks could synthesise their own input -- a WAV with a ramp in the
right channel is a complete test fixture. A network protocol cannot be checked
that way alone, so this phase needs two kinds of check, and the plan is to build
both in M1 rather than discover the need in M4:

- **A fake server in-process**, serving canned Subsonic responses and real audio
  over real HTTP, including the awkward cases: an error envelope with HTTP 200,
  a response with no `Accept-Ranges`, a connection that stalls mid-track, a
  redirect, a 401. This is what makes the failure paths testable at all --
  nobody can unplug a real Navidrome on cue -- and it is what the smoke
  examples (`subsonic_smoke`, `remote_smoke`) and their `/subsonic.html`
  counterparts run against.
- **A real server**, because a fake one only ever returns what its author
  expected. Navidrome is the reference target. This is the half that needs a
  server to exist -- see "What this phase needs from you" below.

## Deferred, on purpose

- **Offline caching.** Keeping played or pinned tracks on disk so they play
  without the server. A real feature with a real design behind it (what is
  cached, what evicts it, how it interacts with the library index), and it wants
  its own milestone or its own phase rather than a corner of this one.
- **One search box over local and server music.** Follows from decision 6. Worth
  doing once both sources exist and the shape of the server rows has settled.
- **Podcasts, internet radio, jukebox mode, shares.** All in the Subsonic API,
  none of them this app's pitch.
- **A native Jellyfin backend** (decision 1).

## Risks, named now

- **Transcoding servers.** Decision 7's fallback covers correctness; what it
  cannot cover is a server configured to transcode everything, where seeking
  means buffering the whole track first. The UI should be honest about that
  state rather than looking hung.
- **Self-signed certificates**, which are the norm on self-hosted servers and
  are rejected by default by rustls and by browsers alike. The browser at least
  has a path (the user accepts the certificate for that origin); the desktop
  shell needs a considered answer rather than an accidental one, and "trust
  everything" is not it.
- **Seek latency over a network.** A container seek re-issues a range request;
  on a slow link that is a visibly slower seek than the local case, on top of
  the ring drain the seek already costs. Worth measuring in M2 rather than
  discovering in use.
- **The password lives in memory** for any server without the API-key
  extension. Decision 4 minimises where it is *stored*; it cannot avoid where it
  is used.
- **API drift between servers.** "Subsonic-compatible" covers a spread of
  versions and half-implemented endpoints. The client should degrade per
  endpoint (a server without `getOpenSubsonicExtensions` answers with an error
  envelope, which must not be fatal) rather than demanding one version.

## What this phase needs from you

A **Navidrome instance to test against** -- the automated checks can be built
against the fake server, but every milestone here ends the way Phase 1's did,
with a live confirmation, and that needs a real server with real music on it. If
there is one already, its URL and an account; if not, a local Docker Navidrome
pointed at the same music the library already scans is enough for all five
milestones. Worth knowing before M1 lands rather than when M2 is ready to prove
itself.

## Sources consulted

- [Subsonic API](https://www.subsonic.org/pages/api.jsp) -- the base protocol:
  required parameters, the token/salt scheme and its worked example, the error
  envelope and its codes, `scrobble`'s parameters.
- [OpenSubsonic API](https://opensubsonic.netlify.app/docs/) and its
  [API-key extension](https://opensubsonic.netlify.app/docs/extensions/apikeyauth/)
  -- the modern spec, backwards-compatible with the above, and the extension
  decision 4 prefers.
- [Navidrome's Subsonic API compatibility](https://www.navidrome.org/docs/developers/subsonic-api/)
  -- the reference server: API v1.16.1 with OpenSubsonic extensions, a global
  CORS handler, and `Accept-Ranges: bytes` with partial content on `stream`.
- [MDN: Cross-Origin-Embedder-Policy](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Cross-Origin-Embedder-Policy)
  -- why a CORS-mode `fetch` is allowed under this app's own `require-corp`
  header while a plain cross-origin `<img src>` is not (M3).
- [JellySonic](https://github.com/lyarenei/JellySonic) -- the Jellyfin Subsonic
  plugin, and its own "experimental" label, behind decision 1.
- [ureq](https://crates.io/crates/ureq) 3.4 -- blocking HTTP with rustls and no
  async runtime, which is what `src-tauri` wants for decision 2's native
  transport: `player-core` has no executor and should not grow one for this.
