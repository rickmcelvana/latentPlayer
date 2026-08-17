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
  examples (`subsonic_smoke`, `remote_smoke`) and their `/subsonic.html` and
  `/remote.html` counterparts run against.
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

## M1 -- Connection and credentials: built, awaiting a live authenticated run

The seam every other milestone calls through. What landed:

**The protocol, once, for both shells.** `player-core/src/subsonic/mod.rs` builds
requests and parses responses and does no IO at all; `subsonic/http.rs` is the
desktop transport (ureq, blocking, native-only beside `playback.rs` and
`library.rs`); `subsonicWeb.ts` is the web one (`fetch`), calling the *same*
Rust through two wasm bindings. Decision 2 held up under the compiler: the
protocol module builds for `wasm32-unknown-unknown` unchanged, so the classifier
that decides "is this the password's fault" is one implementation rather than
two that will drift.

**The trap, handled where it can only be handled once.** A Subsonic failure is
an **HTTP 200** carrying `"status":"failed"` and a code. `parse` is the only way
the module hands back a payload, so there is no path that skips the check. The
test fixtures for it are not invented: they are the bytes this repo's own
Navidrome 0.63.2 returned on 2026-08-17, kept verbatim.

**Auth.** Token auth is `md5(password + salt)`, checked against the spec's own
worked example -- `sesame` + `c19b2d` = `26719a1196d2a940705a59634eb18eab` -- on
both shells, because a wasm md5 that disagreed with the native one would be a
bug nobody would look for in a *hash*. Salts are the shell's (`getrandom`
natively, `crypto.getRandomValues` on the web), 16 bytes each, and a fresh one
per request is asserted rather than assumed.

**Where the secret lives**, per decision 4: the OS credential store natively
(`keyring`, per-platform backend features), IndexedDB on the web -- which is
plaintext, has no alternative in a browser, and is therefore *stated in the
panel where the secret is typed* rather than left for the user to assume. The
address and username are ordinary settings and live beside the library index.
Worth recording early: **this Navidrome does not offer `apiKeyAuthentication`**
(its extensions are transcodeOffset, formPost, songLyrics, indexBasedQueue,
transcoding, playbackReport), so the password path is the one that will actually
be exercised here, not the fallback.

**One decision refined by contact with the code.** Decision 2 says player-core
holds no IO; the native transport is nevertheless *in* player-core, behind
`#[cfg(not(target_arch = "wasm32"))]`. The rule it actually encodes is that the
wasm build has no HTTP client and the protocol module has no opinion about
transport -- both still true. `LocalFile` does file IO in the same crate for the
same reason: a source is a source, and M2's `RemoteSource` will want to live
beside this.

### Verified

- **`cargo test -p player-core --test subsonic_http`: 11/11**, against a fake
  server that misbehaves on purpose -- an error envelope inside a 200, a 502, a
  router login page where JSON should be, a truncated body, a closed port, an
  old server that has never heard of `getOpenSubsonicExtensions` (which must be
  "no extensions", not a failure to connect), and an API key travelling with no
  username beside it. Two of them read what the client put *on the wire*: that
  the token is the hash of the password and the salt that travelled with it, and
  that no two requests share a salt.
- **16 protocol tests** in `subsonic/mod.rs` and 3 in `src-tauri/src/servers.rs`
  (settings round-trip, no secret in the settings file, credential keys scoped
  per server *and* user). Total `player-core` unit tests: **117**, up from 101.
- **Live, native**: `cargo run -p player-core --example subsonic_smoke` against
  the real Navidrome -- a wrong password comes back as `BadCredentials` (which
  also proves the request shape was accepted: a malformed one would be error 10,
  not 40), and the server's real extension list parses.
- **Live, web**: `/subsonic.html`, **8/8** in a real browser against the same
  server -- including the two checks with no native counterpart, that the
  browser can reach it at all (CORS) and that no mixed-content rule applies. The
  server's CORS headers were confirmed directly before any of this was written:
  `Access-Control-Allow-Origin: *` with `Vary: Origin`, and a preflight that
  answers. Decision 5's assumption is now a measurement.
  The eighth check is the one this session could not make for itself, since it
  needs a real account -- **user-run 2026-08-17: "a correct password
  authenticates: navidrome 0.63.2 (be10f89c), API 1.16.1, OpenSubsonic"**. That
  is the token scheme proven end to end: computed here, accepted there.
- **The OS credential store, written to and read back** on the real Windows
  Credential Manager (`servers.rs`'s own test, using an obviously-fake secret
  under a `.invalid` address, and deleting what it wrote). It also asserts that
  forgetting a server leaves nothing behind, which is the whole point of that
  button. The most likely thing to fail silently on a given machine, and the
  failure mode is a password that seems to save and is gone by the next launch.
- **The panel, driven in a browser** through React's own handlers: a scheme-less
  address, a wrong password, and a port with nothing on it each produce a
  message naming the thing to change -- and each leaves the form open with the
  typed values in it. That last part was a bug this driving *found*: the panel
  derived "show the form" from `configured`, and a failed attempt set that flag,
  so a rejected address rendered as a saved server. Both shells now answer
  `configured` from what is stored rather than from what was typed.

### Closed (2026-08-17)

**User-confirmed on both shells.** The web harness reports 8/8 against the real
Navidrome, the eighth check being the one no amount of testing against ourselves
could establish -- "a correct password authenticates: navidrome 0.63.2
(be10f89c), API 1.16.1, OpenSubsonic" -- and the desktop shell's own live
authenticated run passed too. The token is computed by one implementation,
accepted by a real server, and reaches it through both transports.

So M1's seam is real: a connection, credentials that survive a restart in the
place each platform keeps such things, and a client that tells a wrong password
apart from an unreachable server. M2 calls through it.

## M2 -- Remote playback: built, awaiting the web shell's live run

The phase's actual point: a track that lives on the server, played by the same
engine that plays a file on disk.

**Natively, the difference is one source deep.** `player-core/src/remote.rs` is a
`PlaybackSource` over HTTP: `RemoteSource` builds it, `HttpStream` is the
`MediaSource` symphonia reads. One 512KB chunk is held at a time, `seek` moves a
cursor and issues no request at all (symphonia probes a container by seeking
around and reading a few bytes -- fetching on every seek would make opening a
track a dozen round trips), and the fetch happens when a read actually crosses
into bytes not in hand. Everything downstream is untouched: the ring, the EQ, the
tap, every meter, the scope. `load_server_track` is `load_file` with a different
source built, which is exactly what decision 8 asked for.

**The first request settles what kind of server this is.** A 206 with a
`Content-Range` means real ranges: seek freely, hold a chunk. A 200 means the
server ignored the range and sent the whole thing -- decision 7's transcoding
case -- and then the body is buffered whole, bounded at 512MB, and the track
still plays and still seeks. Both are proven against a fake server that does each
on purpose.

**On the web the parity limit is real, and stated rather than hidden**
(decision 5). A browser has no `MediaSource` for the worker to pull through: the
decoder reads a byte array it owns. So a server track is fetched whole before it
plays -- `trackBytes` in `subsonicWeb.ts` -- which costs the wait at the start of
a track and the track's size in memory, and buys back everything else unchanged,
including instant seeking afterwards and a network stall that cannot underrun the
ring because there is no network left to stall. It is also not a *new* cost on
that shell: a picked local file has always been read whole into memory there. The
message when a track is too large names the desktop app, which streams instead.

**Prefetching is what queuing already was.** `queueTrack` does the work when the
track is queued rather than at the seam: natively the decoder opens the stream
when it runs out of the current track (a ring ahead of the listener), and on the
web the whole next track is fetched at that moment. Both are ahead of the seam by
construction, so a gapless hand-off between two *server* tracks needed no new
machinery -- only for the fetching to happen at the right time.

**A network stall is an underrun, and that was already answered.** An empty ring
with the track not ended has meant silence and a held position since Phase 1 M2
(`Shared::finished`'s note in `playback.rs`); a decode error means "this track is
over, move on" rather than a stopped transport. A stalled network lands on the
first, and a network that never recovers lands on the second after ureq's 60s
timeout. What M2 had to add was only that the *source* waits and resumes rather
than corrupting or truncating what it hands the decoder, which is now a test.

**The UI entry point is deliberately small.** "Play random tracks" on the server
panel: `getRandomSongs`, into the same queue local tracks go into, on both
shells. Browsing is M3, and M2 still had to be something a person can press.

### Verified

- **`cargo test -p player-core --test remote_source`: 10/10.** The one that
  matters compares audio: seven seconds of WAV, served over real TCP, decoded
  through `RemoteSource`, against the same bytes decoded locally -- worst
  difference **0.0**, over a fixture whose right channel ramps 0 to 1 so that
  chunks stitched together in the wrong order are detectable rather than merely
  different. Beside it: a container seek to 3.5s of 7s reading 0.501, byte seeks
  either side of a chunk boundary, reading past the end being end-of-stream
  rather than an error, a server that ignores ranges, a 404, a dead port, and a
  server that stalls 250ms per chunk (audio identical, and the delay shows up in
  the time taken, so it passes for the right reason).
- **`/remote.html`, offline half: 3/3** in a real browser -- the stream URL
  carries `format=raw` and the spec's known-answer token and no password, a song
  list parses into playable rows, and a failure inside an HTTP 200 is a failure
  on this side too. The live half is below.
- **Live, native**: `cargo run -p player-core --example remote_smoke` against the
  real Navidrome -- **user-run 2026-08-17, all checks passed**. It picks a track
  at random, opens it, checks the container's duration against the one the server
  stated (a URL built with the wrong id would otherwise play the wrong song
  silently), plays for 1.5s, reads the *analysis tap* to prove audio reached the
  output rather than a counter moving on its own, seeks, and hands over to a
  queued track at the seam.
- The full suite stayed green: 119 `player-core` unit tests, 11 `subsonic_http`,
  4 in `src-tauri`, 16 in `player-render`, clippy and fmt clean.

**Two bugs found here were in the test double, and both looked exactly like
client bugs.** It hung up after every response, so ureq's pooled connection reuse
met a closed socket and the *second* range request of every track died; and on
Windows a socket accepted from a non-blocking listener inherits that, so reads
returned `WouldBlock` before the request had arrived and the server closed
connections mid-request -- intermittent under parallel tests, invisible when run
alone. Both are written up where they happened, in `tests/common/mod.rs`.

### Closed (2026-08-17)

**User-confirmed on both shells.** The native live run (`remote_smoke`) and the
web one (`/remote.html`, in a real browser against the real Navidrome) both pass
every check: a random track opens over the network, the container's length agrees
with the length the server stated, playback advances in real time, the analysis
tap shows audio actually reached the output, a seek lands where it says, and a
queued server track takes over at the seam.

So the phase's point is proven: a track that lives on the server plays through
the same engine, the same DSP path and the same meters as a file on disk, on both
shells -- one over range requests as it decodes, one fetched whole first, and the
difference visible nowhere above `playlist.ts`. M3 can browse a server whose
tracks are already known to play.

## M3 -- Browsing the server: built, awaiting a live run on both shells

M2 could play anything the server offered at random. M3 is how a person finds
something in particular.

**Five endpoints, five payload shapes, one place that knows them.**
`player-core/src/subsonic/browse.rs` holds both halves: `BrowseRequest` says
which endpoint a request is and what parameters it carries, and `parse_browse`
knows where each answer hides its list -- `getArtists` under an alphabetical
index, `getAlbum` with its songs nested inside the album, `search3` returning all
three kinds at once. Both shells call both functions, natively and through wasm,
so no endpoint name or query parameter appears in TypeScript at all. The payload
shapes were read off the OpenSubsonic documentation rather than remembered, and
one of them earns its own note: `getAlbumList2` states an album's name in `name`
and `getArtist`'s nested albums state it in `album`. A client that reads only one
shows a list of blank rows against the other -- a bug that looks like an empty
library -- so both are read and there is a test holding them to each other.

**One shape comes back.** `BrowseResult` is artists, albums and tracks, with
whatever the request had nothing to say about left empty. That is what lets the
same rows draw a search result and a browse, and it is why the UI's five views
are one component rather than five.

**Cover art, and the two different things it means.** Natively it is served over
the same `cover://` scheme M7 built for the local library, with a `server:`
prefix on the id -- the component asks for a URL and gets one, exactly as before.
But the handler is now **asynchronous**, and that is not tidying: a server cover
is fetched, and answering on the main thread would mean the whole UI waiting on
the network once per row. Fetched covers are held in a byte-bounded cache for the
session, cleared when the server changes, because the webview asks again every
time a row is drawn. On the web there is no `cover://`, and the obvious thing --
pointing an `<img src>` at the server -- is blocked by this app's own
`Cross-Origin-Embedder-Policy: require-corp` before the network sees it, since a
plain cross-origin image load is a no-cors request. So the web shell fetches the
art in CORS mode and wraps it in a blob URL, cached by promise for the reason
M7's local covers are (rows render together, and a cache written after the fetch
resolves is still empty when the third caller looks).

**The library panel grew a source switch, and the rows did not change.** Local
and server music are genuinely different underneath -- a scanned index that is
filtered in memory, versus a server asked live (decision 6) -- so the server's
half is its own component. What they share is what the listener sees, which is
now literally one implementation: `Rows.tsx` draws every row in the app, and
clicking one means the same thing on either side (play the list you can see, from
where you clicked). Searching the server is debounced, because unlike the local
search it is a request to someone's machine rather than a filter over an array.

### Verified

- **`cargo test -p player-core --test subsonic_browse`: 7/7** against the fake
  server -- what went on the wire for a page of albums (endpoint, `type`, `size`,
  `offset`, a token and no password), an album's tracks coming back with the
  suffix playback needs, a search sending all three counts with an ampersand in
  the query correctly escaped, a failure envelope inside an HTTP 200 that must
  not read as an empty library, and the three ways cover art can answer: an
  image, a Subsonic error, and a proxy's login page.
- **11 new unit tests in `browse.rs`**, holding the five payload shapes to known
  answers -- including the `name`/`album` case above and a server with no artists
  at all, which answers `{"artists":{}}` rather than with an empty index.
  `player-core` unit tests: **129**, up from 119.
- **`/browse.html`, offline half: 3/3** in a real browser -- a browse URL naming
  its endpoint and parameters with no password in it, artists read out of the
  index, an album carrying its tracks.
- The full suite stayed green: 129 `player-core` unit tests, 10 `remote_source`,
  11 `subsonic_http`, 7 `subsonic_browse`, 4 in `src-tauri`, 16 in
  `player-render`, clippy and fmt clean, `tsc` and oxlint clean.

### What remains before M3 closes

The live runs, on both shells, against the real Navidrome:
`cargo run -p player-core --example browse_smoke` and `/browse.html`. Between
them they check the same claims -- artists, a page of albums and the page after
it being different music, an album's tracks, a search that finds something and a
search that finds nothing, and that a browsed track is one this app can actually
play -- plus, on the web only, that cover art fetched from another origin really
decodes under this app's COEP header.
