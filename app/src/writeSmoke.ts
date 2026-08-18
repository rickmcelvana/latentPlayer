// Live check of *writing* to the server from a browser (Phase 2 M4) -- the web
// counterpart of `cargo run -p player-core --example write_smoke` and
// `cargo test -p player-core --test subsonic_write`. Served at /write.html; not
// part of the production bundle.
//
// It drives server.ts, the same adapter the app's own server tab drives, so a
// pass here is a statement about the app rather than about the harness.
//
// The claim this file exists to support is the milestone's own: the two shells
// write the same things and count a play the same way. So the offline half
// checks the scrobble rule *in the browser's wasm copy* of the very tracker the
// desktop shell holds natively, driven with the same numbers Rust's own tests
// use -- if those two ever disagreed, this is where it would show.
//
// # What this leaves behind
//
// Nothing, deliberately, except one thing it will not do without being asked.
// The playlist it creates is deleted at the end and every star and rating is put
// back. Submitting a *play* writes to a listening history nothing here can edit,
// so that check runs only when the box is ticked.
//
// The server it uses is the one the app's own panel stored. There is nowhere to
// type a password here: connect in the app first, then run this.
import init, { WasmScrobbler } from "./wasm/player_rt_wasm.js";
import {
  serverBrowse,
  serverMark,
  serverPlaylists,
  serverState,
  type AlbumRow,
  type ServerTrack,
} from "./server";

const out = document.getElementById("out") as HTMLPreElement;
let passed = 0;
let failed = 0;

function check(label: string, ok: boolean, detail: string) {
  if (ok) passed++;
  else failed++;
  out.textContent += `  [${ok ? "PASS" : "FAIL"}] ${label}: ${detail}\n`;
}

function skip(label: string, why: string) {
  out.textContent += `  [SKIP] ${label}: ${why}\n`;
}

/// One track as the server states it now, by asking for its album again. The
/// same route `write_smoke.rs` takes, and for the same reason: an album is how
/// the app itself arrives at a track.
async function rereadTrack(albumId: string, trackId: string): Promise<ServerTrack | undefined> {
  const album = await serverBrowse({ kind: "album", id: albumId });
  return album.tracks.find((track) => track.id === trackId);
}

/// The scrobble rule, checked against the browser's own copy of the tracker.
///
/// No network, no server, and no threshold written in TypeScript -- these are
/// the same numbers `scrobble.rs`'s tests drive the native tracker with, so a
/// pass here says the two shells agree about what a listener has heard.
function checkTheRule() {
  const tracker = new WasmScrobbler();

  // A minute-long track announces itself once, and submits at half of it.
  const first = tracker.observe("s1", 0, 60, true, 0);
  check(
    "a track that starts playing announces itself once",
    first.length === 1 && first[0] === "nowPlaying:s1",
    JSON.stringify(first),
  );

  let submissions: string[] = [];
  let announced = 0;
  for (let second = 1; second <= 60; second++) {
    for (const action of tracker.observe("s1", second, 60, true, second * 1000)) {
      if (action.startsWith("submit:")) submissions.push(`${action}@${second}s`);
      else announced++;
    }
  }
  check(
    "a play is submitted once, at half the track",
    submissions.length === 1 && submissions[0].endsWith("@30s"),
    JSON.stringify(submissions),
  );
  check("now-playing is not repeated per poll", announced === 0, `${announced} extra`);

  // Seeking to the end is not listening to it -- the check the whole state
  // machine exists for.
  const seeker = new WasmScrobbler();
  seeker.observe("s2", 0, 300, true, 0);
  seeker.observe("s2", 1, 300, true, 1000);
  seeker.observe("s2", 2, 300, true, 2000);
  const afterSeek = seeker.observe("s2", 299, 300, true, 2100);
  check(
    "seeking to the end of a track does not earn a play",
    afterSeek.length === 0 && Math.abs(seeker.heardSeconds() - 2) < 1e-6,
    `${seeker.heardSeconds().toFixed(3)}s heard, actions ${JSON.stringify(afterSeek)}`,
  );

  // A pause credits nothing, however long it lasts.
  const paused = new WasmScrobbler();
  paused.observe("s3", 0, 100, true, 0);
  paused.observe("s3", 10, 100, true, 10_000);
  paused.observe("s3", 10, 100, false, 3_610_000);
  const resumed = paused.observe("s3", 11, 100, true, 3_611_000);
  check(
    "an hour paused is not an hour listened",
    resumed.length === 0 && Math.abs(paused.heardSeconds() - 11) < 1e-6,
    `${paused.heardSeconds().toFixed(3)}s heard`,
  );

  // A track under thirty seconds never counts, however many times it is played.
  const tiny = new WasmScrobbler();
  tiny.observe("s4", 0, 20, true, 0);
  let tinySubmissions = 0;
  for (let second = 1; second <= 20; second++) {
    for (const action of tiny.observe("s4", second, 20, true, second * 1000)) {
      if (action.startsWith("submit:")) tinySubmissions++;
    }
  }
  check(
    "a track under thirty seconds never submits",
    tinySubmissions === 0,
    `${tinySubmissions} submissions after playing it whole`,
  );
}

async function run() {
  out.textContent = "";
  passed = 0;
  failed = 0;
  await init();

  // --- the rule, with no network involved ----------------------------------
  checkTheRule();

  const state = await serverState();
  if (!state.connected) {
    skip("everything else", "no server is connected -- set one up in the app first");
    out.textContent += `\n${passed} passed, ${failed} failed\n`;
    return;
  }
  out.textContent += `  server: ${state.serverType} ${state.serverVersion} at ${state.baseUrl}\n`;

  // --- something real to write about ---------------------------------------
  const albums = await serverBrowse({ kind: "albums", sort: "alphabeticalByName", size: 1, offset: 0 });
  const album: AlbumRow | undefined = albums.albums[0];
  if (!album) {
    skip("the rest", "the server has no albums to write about");
    out.textContent += `\n${passed} passed, ${failed} failed\n`;
    return;
  }
  const opened = await serverBrowse({ kind: "album", id: album.id });
  const tracks = opened.tracks;
  if (tracks.length === 0) {
    skip("the rest", `"${album.name}" has no tracks`);
    out.textContent += `\n${passed} passed, ${failed} failed\n`;
    return;
  }
  out.textContent += `  using "${album.name}" by ${album.artist} (${tracks.length} tracks)\n`;

  // --- playlists ------------------------------------------------------------
  const before = (await serverPlaylists({ kind: "list" })).playlists;
  check("the server's playlists come back", true, `${before.length} playlists`);

  const name = `latentPlayer writeSmoke ${Date.now()}`;
  const firstTwo = tracks.slice(0, 2).map((track) => track.id);
  const created = await serverPlaylists({ kind: "create", name, songIds: firstTwo });
  const playlistId = created.playlists[0]?.id;
  if (!playlistId) {
    check("a playlist is created", false, "the server returned no playlist to work with");
    out.textContent += `\n${passed} passed, ${failed} failed\n`;
    return;
  }
  // The repeated-`songId` check, live. A browser building this URL itself would
  // most likely send a comma-joined list and get a playlist of nothing.
  check(
    "a playlist is created holding every track it was given",
    created.tracks.length === firstTwo.length,
    `asked for ${firstTwo.length}, the server made a playlist of ${created.tracks.length}`,
  );

  const listed = (await serverPlaylists({ kind: "list" })).playlists;
  check(
    "the new playlist is in the list afterwards",
    listed.some((playlist) => playlist.id === playlistId),
    `${listed.length} playlists, was ${before.length}`,
  );

  if (tracks.length > 2) {
    const extra = tracks.slice(2, 4).map((track) => track.id);
    await serverPlaylists({ kind: "addSongs", id: playlistId, songIds: extra });
    const now = (await serverPlaylists({ kind: "get", id: playlistId })).tracks;
    check(
      "tracks can be added to a playlist",
      now.length === firstTwo.length + extra.length,
      `${now.length} tracks after adding ${extra.length}`,
    );
  }

  const held = (await serverPlaylists({ kind: "get", id: playlistId })).tracks;
  if (held.length > 1) {
    const dropped = held[0].id;
    await serverPlaylists({ kind: "removeIndices", id: playlistId, indices: [0] });
    const left = (await serverPlaylists({ kind: "get", id: playlistId })).tracks;
    check(
      "a track can be removed by position, and it is the right one",
      left.length === held.length - 1 && !left.some((track) => track.id === dropped),
      `${left.length} tracks left, was ${held.length}`,
    );
  }

  const renamed = `${name} (renamed)`;
  await serverPlaylists({ kind: "rename", id: playlistId, name: renamed });
  const after = (await serverPlaylists({ kind: "get", id: playlistId })).playlists[0];
  check(
    "a playlist can be renamed",
    after?.name === renamed,
    `the server calls it "${after?.name ?? "nothing"}"`,
  );

  // --- stars and ratings, each put back afterwards --------------------------
  const track = tracks[0];
  const wasStarred = track.starred;
  await serverMark({ kind: wasStarred ? "unstar" : "star", target: "song", id: track.id });
  const starredNow = (await rereadTrack(album.id, track.id))?.starred;
  check(
    "starring a track is recorded by the server",
    starredNow === !wasStarred,
    `was ${wasStarred}, now ${starredNow}`,
  );
  await serverMark({ kind: wasStarred ? "star" : "unstar", target: "song", id: track.id });

  // The parameter-name decision itself, in front of a real server: an ID3
  // album is `albumId`, and sent as `id` the request succeeds and does nothing.
  const albumWasStarred = album.starred;
  await serverMark({
    kind: albumWasStarred ? "unstar" : "star",
    target: "album",
    id: album.id,
  });
  const albumNow = (await serverBrowse({ kind: "album", id: album.id })).albums[0]?.starred;
  check(
    "starring an album is recorded -- i.e. albumId is the right parameter",
    albumNow === !albumWasStarred,
    `was ${albumWasStarred}, now ${albumNow}`,
  );
  await serverMark({
    kind: albumWasStarred ? "star" : "unstar",
    target: "album",
    id: album.id,
  });

  const wasRated = track.rating;
  const wanted = wasRated === 4 ? 3 : 4;
  await serverMark({ kind: "setRating", id: track.id, rating: wanted });
  const ratedNow = (await rereadTrack(album.id, track.id))?.rating;
  check(
    "a rating is recorded by the server",
    ratedNow === wanted,
    `asked for ${wanted}, the server says ${ratedNow}`,
  );
  await serverMark({ kind: "setRating", id: track.id, rating: wasRated ?? 0 });

  // --- scrobbling -----------------------------------------------------------
  //
  // Through `scrobble` directly rather than through the tracker: what is being
  // checked here is that the server accepts what this shell sends, and the rule
  // that decides *when* to send it was already checked above with no network in
  // the way.
  const { scrobble } = await import("./subsonicWeb");
  await scrobble(track.id, false, null);
  check('"now playing" is accepted', true, `announced "${track.title}"`);

  const allowPlay = (document.getElementById("allow-play") as HTMLInputElement | null)?.checked;
  if (allowPlay) {
    await scrobble(track.id, true, Date.now());
    check("a play is accepted", true, `submitted "${track.title}" -- this one is not undone`);
  } else {
    skip(
      "submitting a play",
      "tick the box to allow it. It adds to a listening history nothing here can take back",
    );
  }

  // --- and put the library back ---------------------------------------------
  await serverPlaylists({ kind: "delete", id: playlistId });
  const left = (await serverPlaylists({ kind: "list" })).playlists;
  check(
    "the playlist is deleted afterwards, leaving the library as it was",
    !left.some((playlist) => playlist.id === playlistId),
    `${left.length} playlists, was ${before.length} before this ran`,
  );

  out.textContent += `\n${passed} passed, ${failed} failed\n`;
}

document.getElementById("run")?.addEventListener("click", () => {
  run().catch((e) => {
    out.textContent += `\n  [FAIL] the harness itself threw: ${String(e)}\n`;
  });
});
