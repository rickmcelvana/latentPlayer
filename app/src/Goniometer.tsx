// The goniometer: a Lissajous of the stereo field, drawn from points the
// engine hands over.
//
// The transform that makes it readable -- rotating L/R onto mid/side -- happens
// in Rust for both shells (M5 decision 7), so this file plots pairs and nothing
// else. Mono material stands up the centre line, an out-of-phase pair lies
// flat, and a wide mix fills the middle.
//
// Canvas 2D, like the spectrum and for the same reason: player-render's wgpu
// pipeline exists for the 3D vectorscope (M6), and a second render surface in
// the desktop shell buys nothing here.
import { useEffect, useRef } from "react";
import { goniometer } from "./analysis";

/// What the display treats as the edge. The points are real mid/side
/// amplitudes, so full-scale mono reaches sqrt(2) -- drawing to 1.0 would clip
/// exactly the material a goniometer is most often pointed at.
const FULL_SCALE = Math.SQRT2;

interface Props {
  /// Only polls while there is something to plot.
  active: boolean;
  size?: number;
}

export function Goniometer({ active, size = 160 }: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    if (!active) return;
    const canvas = canvasRef.current;
    const ctx = canvas?.getContext("2d");
    if (!canvas || !ctx) return;

    let frame = 0;
    let stopped = false;
    // One request at a time; see Spectrum.tsx for why (every frame is an IPC
    // round trip on the desktop shell).
    let inFlight = false;

    const centre = size / 2;
    const scale = (size / 2 - 6) / FULL_SCALE;

    const draw = (points: Float32Array) => {
      ctx.fillStyle = "#050508";
      ctx.fillRect(0, 0, size, size);

      // The axes a goniometer is read against: vertical is mono, horizontal is
      // the difference between the channels, and the diagonals are the
      // individual channels (hard left and hard right).
      ctx.strokeStyle = "#1c1c26";
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(centre, 4);
      ctx.lineTo(centre, size - 4);
      ctx.moveTo(4, centre);
      ctx.lineTo(size - 4, centre);
      ctx.stroke();

      ctx.strokeStyle = "#14141c";
      ctx.beginPath();
      ctx.moveTo(6, 6);
      ctx.lineTo(size - 6, size - 6);
      ctx.moveTo(size - 6, 6);
      ctx.lineTo(6, size - 6);
      ctx.stroke();

      // Points rather than a joined trace: at 256 decimated samples a polyline
      // would draw chords across the figure that the audio never took.
      ctx.fillStyle = "#26f28c";
      for (let i = 0; i + 1 < points.length; i += 2) {
        // y is inverted because canvas y grows downwards, and mono should
        // point up.
        const x = centre + points[i] * scale;
        const y = centre - points[i + 1] * scale;
        ctx.fillRect(x - 0.75, y - 0.75, 1.5, 1.5);
      }
    };

    const tick = () => {
      if (stopped) return;
      frame = requestAnimationFrame(tick);
      if (inFlight) return;
      inFlight = true;
      goniometer()
        .then((points) => {
          inFlight = false;
          if (!stopped) draw(points);
        })
        .catch(() => {
          inFlight = false;
        });
    };
    frame = requestAnimationFrame(tick);

    return () => {
      stopped = true;
      cancelAnimationFrame(frame);
    };
  }, [active, size]);

  return <canvas ref={canvasRef} width={size} height={size} className="goniometer" />;
}
