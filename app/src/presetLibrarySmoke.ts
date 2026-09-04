// Live check of the web preset library: parsing through wasm, the IndexedDB
// store, the scan worker, and selection (Phase 3 M7c item 3).
//
// The web counterpart of `cargo run -p player-render --example
// preset_library_smoke`, running the same checks against the same claims so the
// two shells can be held to one standard. Served at /preset.html by the dev
// server; not part of the production bundle.
//
// The files are synthesised here, in the same shape the native example builds
// them: a good preset, one that names a texture pack, one whose shader calls a
// function this build does not know, and one that is not a preset at all.
import {
  allPresetFolders,
  allPresets,
  clearPresets,
  deletePresetFolder,
  replacePresetFolder,
} from "./presetStore";
import type { PresetDoneResponse, PresetProgressResponse, PresetScanFile } from "./presetWorker";

const out = document.getElementById("out") as HTMLPreElement;
let failures = 0;

function log(line: string): void {
  out.textContent += `${line}\n`;
}

function check(label: string, ok: boolean, detail: string): void {
  if (!ok) failures += 1;
  log(`  [${ok ? "PASS" : "FAIL"}] ${label}: ${detail}`);
}

/// Runs one scan through the real preset worker -- the same code path
/// `presetLibrary.ts` drives, off the main thread.
function runWorker(folderId: string, files: PresetScanFile[]): Promise<PresetDoneResponse> {
  return new Promise((resolve, reject) => {
    const worker = new Worker(new URL("./presetWorker.ts", import.meta.url), { type: "module" });
    worker.onmessage = (
      e: MessageEvent<PresetDoneResponse | PresetProgressResponse | { type: "error"; message: string }>,
    ) => {
      if (e.data.type === "progress") return;
      worker.terminate();
      if (e.data.type === "error") reject(new Error(e.data.message));
      else resolve(e.data);
    };
    worker.onerror = (event) => {
      worker.terminate();
      reject(new Error(event.message || "the preset worker failed"));
    };
    worker.postMessage({ type: "scan", folderId, files });
  });
}

function file(name: string, text: string): PresetScanFile {
  return { relativePath: [name], file: new File([text], name, { type: "text/plain" }) };
}

async function run(): Promise<void> {
  out.textContent = "";
  failures = 0;
  await clearPresets();

  const files: PresetScanFile[] = [
    file("good.milk", "[preset00]\nfDecay=0.9\n"),
    file(
      "textured.milk",
      "[preset00]\nwarp_1=`sampler sampler_fw_clouds;\nwarp_2=`shader_body\nwarp_3=`{\nwarp_4=`ret = tex2D(sampler_fw_clouds, uv).xyz;\nwarp_5=`}\n",
    ),
    file(
      "needy.milk",
      "[preset00]\nwarp_1=`shader_body\nwarp_2=`{\nwarp_3=`ret = nosuchfunction(uv);\nwarp_4=`}\n",
    ),
    file("not-a-preset.milk", "zoom=1\n"),
  ];

  log(`preset library smoke -- scanning ${files.length} synthesised files`);

  const folderId = "presets:Test";
  const scanned = await runWorker(folderId, files);

  check(
    "the worker reads every file it is given",
    scanned.found === 4 && scanned.added === 4 && scanned.failed === 0,
    `found ${scanned.found}, added ${scanned.added}, failed ${scanned.failed}`,
  );

  const good = scanned.presets.find((p) => p.fileName === "good.milk");
  check(
    "a clean preset has an empty report",
    good?.report.length === 0,
    `${good?.report.length} line(s): ${JSON.stringify(good?.report)}`,
  );

  const needy = scanned.presets.find((p) => p.fileName === "needy.milk");
  check(
    "a preset with an unknown function names it in its report",
    needy?.report.some((l) => l.includes("nosuchfunction")) === true,
    JSON.stringify(needy?.report),
  );

  const notAPreset = scanned.presets.find((p) => p.fileName === "not-a-preset.milk");
  check(
    "a file that is not a preset is listed as failing rather than hidden",
    notAPreset?.report.some((l) => l.includes("not a MilkDrop preset")) === true,
    JSON.stringify(notAPreset?.report),
  );

  const textured = scanned.presets.find((p) => p.fileName === "textured.milk");
  check(
    "a preset's texture-pack names are carried for selection",
    textured?.textures.includes("clouds") === true,
    JSON.stringify(textured?.textures),
  );

  // Into the store, which is where the web build's preset library lives.
  await replacePresetFolder({ id: folderId, name: "Test", handle: null }, scanned.presets);

  const stored = await allPresets();
  check(
    "the rows persist in IndexedDB",
    stored.length === 4 && (await allPresetFolders()).length === 1,
    `${stored.length} rows and ${(await allPresetFolders()).length} folder(s) read back`,
  );

  // A rescan must be idempotent -- scan twice, do not get a library twice the
  // size.
  const rescanned = await runWorker(folderId, files);
  await replacePresetFolder({ id: folderId, name: "Test", handle: null }, rescanned.presets);
  const afterRescan = await allPresets();
  check(
    "rescanning updates rows rather than duplicating them",
    afterRescan.length === 4 &&
      JSON.stringify(afterRescan.map((p) => p.id).sort()) ===
        JSON.stringify(stored.map((p) => p.id).sort()),
    `${stored.length} rows before, ${afterRescan.length} after, same ids`,
  );

  await deletePresetFolder(folderId);
  check(
    "forgetting a folder drops its rows",
    (await allPresets()).length === 0 && (await allPresetFolders()).length === 0,
    `${(await allPresets()).length} rows left`,
  );

  log(failures === 0 ? "\nALL PASS" : `\n${failures} FAILED`);
}

document.getElementById("run")?.addEventListener("click", () => {
  void run().catch((e) => log(`ERROR: ${e}`));
});
