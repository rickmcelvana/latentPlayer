// Live check of browsing the server from a browser (Phase 2 M3) -- the web
// counterpart of `cargo run -p player-core --example browse_smoke` and
// `cargo test -p player-core --test subsonic_browse`. Served at /browse.html;
// not part of the production bundle.
//
// It drives server.ts, the same adapter the library panel's server tab drives,
// so a pass here is a statement about the app rather than about the harness.
//
// Two of these checks have no native counterpart. Cover art is fetched and
// wrapped in a blob URL rather than pointed at directly, because this app sets
// Cross-Origin-Embedder-Policy: require-corp and a plain cross-origin <img src>
// is a no-cors load that the header blocks before the network sees it -- so the
// check that art *renders* can only be made here. And a browsed track is played
// through the whole web path, which is the claim M3 exists to support: browsing
// finds music that this shell can actually play.
//
// The server it uses is the one the app's own panel stored. There is nowhere to
// type a password here: connect in the app first, then run this.
import init, { subsonicBrowseResult, subsonicBrowseUrl } from "./wasm/player_rt_wasm.js";
import { scopeWindow } from "./playerEngine";
import { loadTrack, readTransport, setVolume, trackFromServer } from "./playlist";
import { serverBrowse, serverCoverUrl, serverState, type AlbumRow } from "./server";

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

const wait = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/// One album with two tracks in it, as a server states it.
const CANNED_ALBUM = new TextEncoder().encode(
  '{"subsonic-response":{"status":"ok","version":"1.16.1","album":{' +
    '"id":"10","name":"Kind of Blue","artist":"Miles Davis","coverArt":"al-10","songCount":2,' +
    '"duration":1234,"year":1959,"song":[' +
    '{"id":"100","title":"So What","artist":"Miles Davis","album":"Kind of Blue",' +
    '"suffix":"flac","duration":545,"track":1,"coverArt":"al-10"},' +
    '{"id":"101","title":"Blue in Green","artist":"Miles Davis","album":"Kind of Blue",' +
    '"suffix":"flac","duration":337,"track":2}]}}}',
);

/// The alphabetical index `getArtists` answers with -- the shape a client is
/// most likely to read wrongly, and reading it wrongly looks like an empty
/// library rather than an error.
const CANNED_ARTISTS = new TextEncoder().encode(
  '{"subsonic-response":{"status":"ok","version":"1.16.1","artists":{"index":[' +
    '{"name":"C","artist":[{"id":"1","name":"Cocteau Twins","coverArt":"ar-1","albumCount":8}]},' +
    '{"name":"M","artist":[{"id":"2","name":"Miles Davis","albumCount":21},' +
    '{"id":"3","name":"Mogwai","albumCount":10}]}]}}}',
);

async function run() {
  out.textContent = "";
  passed = 0;
  failed = 0;
  await init();

  // --- the protocol, with no network involved --------------------------------
  const url = subsonicBrowseUrl(
    "http://example.com",
    "rick",
    "sesame",
    false,
    "c19b2d",
    JSON.stringify({ kind: "albums", sort: "alphabeticalByName", size: 60, offset: 60 }),
  );
  check(
    "a browse URL names the endpoint and its parameters",
    url.includes("/rest/getAlbumList2") &&
      url.includes("type=alphabeticalByName") &&
      url.includes("size=60") &&
      url.includes("offset=60") &&
      !url.includes("sesame"),
    url.replace(/^https?:\/\/example\.com/, ""),
  );

  const artists = JSON.parse(
    subsonicBrowseResult(JSON.stringify({ kind: "artists" }), CANNED_ARTISTS),
  );
  check(
    "artists are read out of the alphabetical index",
    artists.ok === true &&
      artists.artists.length === 3 &&
      artists.artists[0].name === "Cocteau Twins" &&
      artists.artists[1].albumCount === 21,
    (artists.artists ?? []).map((a: { name: string }) => a.name).join(", "),
  );

  const album = JSON.parse(
    subsonicBrowseResult(JSON.stringify({ kind: "album", id: "10" }), CANNED_ALBUM),
  );
  check(
    "an album carries its tracks, with what playback needs",
    album.ok === true &&
      album.tracks.length === 2 &&
      album.tracks[0].suffix === "flac" &&
      album.tracks[0].durationSeconds === 545 &&
      album.albums[0].name === "Kind of Blue",
    `${album.albums?.[0]?.name}: ${(album.tracks ?? []).map((t: { title: string }) => t.title).join(", ")}`,
  );

  // --- and now the server ----------------------------------------------------
  const state = await serverState();
  if (!state.configured) {
    skip("the live checks", "no server is set up -- connect one in the app, then run this");
    return summary();
  }
  out.textContent += `server:  ${state.baseUrl} as ${state.user}\n`;

  let albums: AlbumRow[];
  try {
    const page = await serverBrowse({
      kind: "albums",
      sort: "alphabeticalByName",
      size: 10,
      offset: 0,
    });
    albums = page.albums;
    check(
      "a page of albums comes back from the real server",
      albums.length > 0,
      albums.length > 0
        ? `${albums.length} albums, first: ${albums[0]!.artist} -- ${albums[0]!.name}`
        : "the server returned no albums",
    );
  } catch (e) {
    check("a page of albums comes back from the real server", false, String(e));
    return summary();
  }
  if (albums.length === 0) return summary();

  try {
    const found = await serverBrowse({ kind: "artists" });
    check(
      "the server's artists come back",
      found.artists.length > 0,
      `${found.artists.length} artists, first: ${found.artists[0]?.name ?? "none"}`,
    );
  } catch (e) {
    check("the server's artists come back", false, String(e));
  }

  const first = albums[0]!;
  let contents;
  try {
    contents = await serverBrowse({ kind: "album", id: first.id });
    check(
      "an album's tracks come back",
      contents.tracks.length > 0,
      `${contents.tracks.length} tracks in "${first.name}", first: ${contents.tracks[0]?.title ?? "none"}`,
    );
  } catch (e) {
    check("an album's tracks come back", false, String(e));
    return summary();
  }

  // Cover art, and specifically that it *renders*: fetched in CORS mode and
  // wrapped in a blob URL, because this page's own COEP header blocks the
  // straightforward cross-origin <img src>. Decoding it is the proof -- a URL
  // that resolves to an error page would still be a URL.
  const coverId = first.coverArt ?? contents.tracks[0]?.coverArt ?? null;
  if (coverId) {
    try {
      const url = await serverCoverUrl(coverId);
      const drawn = url ? await decodes(url) : false;
      check(
        "cover art fetches and decodes as an image (COEP allows it)",
        drawn,
        url ? `${url.slice(0, 24)}… decoded` : "no art came back",
      );
    } catch (e) {
      check("cover art fetches and decodes as an image (COEP allows it)", false, String(e));
    }
  } else {
    skip("the cover art check", "this album has no art on the server");
  }

  try {
    const needle = first.name.split(/\s+/)[0] ?? first.name;
    const results = await serverBrowse({ kind: "search", query: needle, count: 10 });
    check(
      "searching the server finds something it was asked about",
      results.artists.length + results.albums.length + results.tracks.length > 0,
      `"${needle}": ${results.artists.length} artists, ${results.albums.length} albums, ${results.tracks.length} tracks`,
    );
    const nothing = await serverBrowse({
      kind: "search",
      query: "zzzzz-not-in-any-library-zzzzz",
      count: 5,
    });
    check(
      "a search that matches nothing is empty rather than an error",
      nothing.artists.length + nothing.albums.length + nothing.tracks.length === 0,
      "no results, no error",
    );
  } catch (e) {
    check("searching the server finds something it was asked about", false, String(e));
  }

  // --- browsing found music this shell can play ------------------------------
  const track = contents.tracks[0];
  if (!track) return summary();
  setVolume(0.1); // be kind to whoever is at the keyboard
  try {
    const loaded = await loadTrack(trackFromServer(track));
    await wait(1200);
    const playing = await readTransport();
    const window = scopeWindow(4096);
    let peak = 0;
    for (const sample of window) peak = Math.max(peak, Math.abs(sample));
    check(
      "a browsed track plays, and real audio reaches the output",
      playing.position > 0.5 && peak > 0,
      `${playing.position.toFixed(2)}s in, peak ${peak.toFixed(4)}, ${loaded.duration.toFixed(1)}s long`,
    );
    // The server states a length in the browse row and the container states one
    // when the file is decoded. Disagreeing means the row and the audio are not
    // the same track, which a wrong id would do silently.
    check(
      "the track that played is the track that was browsed to",
      track.durationSeconds === 0 ||
        loaded.duration === 0 ||
        Math.abs(loaded.duration - track.durationSeconds) <= 2,
      `the row says ${track.durationSeconds}s, the audio is ${loaded.duration.toFixed(1)}s`,
    );
  } catch (e) {
    check("a browsed track plays, and real audio reaches the output", false, String(e));
  }

  summary();
}

/// Whether a URL really is an image, by decoding it.
function decodes(url: string): Promise<boolean> {
  return new Promise((resolve) => {
    const image = new Image();
    image.onload = () => resolve(image.naturalWidth > 0);
    image.onerror = () => resolve(false);
    image.src = url;
  });
}

function summary() {
  out.textContent += `\n${passed}/${passed + failed} checks passed\n`;
}

document.getElementById("run")?.addEventListener("click", () => {
  void run().catch((e) => {
    out.textContent += `\nthe harness itself failed: ${e}\n`;
  });
});
