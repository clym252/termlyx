export const FONT_CANDIDATES: readonly string[] = [
  "JetBrains Mono", "SF Mono", "Menlo", "Monaco", "Cascadia Mono", "Cascadia Code", "Consolas",
  "Courier New", "DejaVu Sans Mono", "Ubuntu Mono", "Roboto Mono", "Noto Sans Mono", "Fira Code",
];

const BUNDLED = "JetBrains Mono";
const SAMPLE = "mmmmmmmmmmlli1WWWW@#";

/** A family is installed if text set in it measures differently from both generic fallbacks. */
export function canvasCheck(family: string): boolean {
  try {
    const ctx = document.createElement("canvas").getContext("2d");
    if (!ctx) return false;
    const width = (font: string) => {
      ctx.font = `72px ${font}`;
      return ctx.measureText(SAMPLE).width;
    };
    return (
      width(`"${family}", monospace`) !== width("monospace") ||
      width(`"${family}", serif`) !== width("serif")
    );
  } catch {
    return false;
  }
}

let cached: string[] | null = null;

export function installedFonts(isInstalled?: (family: string) => boolean): string[] {
  const run = (check: (f: string) => boolean) => FONT_CANDIDATES.filter((f) => f === BUNDLED || check(f));
  if (isInstalled) return run(isInstalled);
  return (cached ??= run(canvasCheck)); // measured once per session
}

export const stackFor = (family: string): string => `"${family}", monospace`;

export function familyOf(stack: string): string {
  return stack.split(",")[0].trim().replace(/^["']|["']$/g, "");
}
