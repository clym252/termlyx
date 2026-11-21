import { useMemo } from "react";
import qrcode from "qrcode-generator";

/** Quiet zone around the symbol, in modules (the QR standard asks for 4). */
const QUIET = 4;

/** One closed square per dark module, as a single SVG path in module units. */
export function qrPath(text: string): { path: string; size: number } {
  // Type 0 picks the smallest version that fits; M keeps a little redundancy for a screen.
  const qr = qrcode(0, "M");
  qr.addData(text);
  qr.make();
  const count = qr.getModuleCount();
  let path = "";
  for (let row = 0; row < count; row++) {
    for (let col = 0; col < count; col++) {
      if (qr.isDark(row, col)) path += `M${col + QUIET} ${row + QUIET}h1v1h-1z`;
    }
  }
  return { path, size: count + 2 * QUIET };
}

/**
 * A QR code of `text`, drawn as an inline SVG. It is made on this device while
 * rendering (no network, no image URL) and is a pure function of `text`:
 * nothing is stored, and the text itself is not written into the image. Black
 * on white in both themes, since scanners need that contrast.
 */
export function QrCode({ text, label }: { text: string; label: string }) {
  const { path, size } = useMemo(() => qrPath(text), [text]);
  return (
    <svg
      className="qr-code"
      role="img"
      aria-label={label}
      viewBox={`0 0 ${size} ${size}`}
      shapeRendering="crispEdges"
      xmlns="http://www.w3.org/2000/svg"
    >
      <rect width={size} height={size} fill="#fff" />
      <path d={path} fill="#000" />
    </svg>
  );
}
