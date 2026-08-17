// Formatting shared by the views. Its own module rather than a corner of a
// component file, so a component file exports components and nothing else --
// which is also what keeps fast refresh working on them.

/// mm:ss, or an em dash when nothing said how long it is.
///
/// The em dash is a real distinction rather than tidiness: a VBR MP3 with no
/// Xing header states no duration locally, and a server may state none either.
/// Rendering that as "0:00" would be a different claim -- that it is empty.
export function formatDuration(seconds: number | null): string {
  if (seconds === null || !Number.isFinite(seconds) || seconds <= 0) return "—";
  const total = Math.round(seconds);
  return `${Math.floor(total / 60)}:${(total % 60).toString().padStart(2, "0")}`;
}
