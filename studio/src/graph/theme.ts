// The design tokens the graph canvas paints with, read from CSS.

import { useEffect, useState } from "react";

import type { Palette } from "./engine.ts";

export function cssVar(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

/** The engine's colors and typefaces, from the current theme. */
export function enginePalette(): Palette {
  return {
    text: cssVar("--text"),
    muted: cssVar("--muted"),
    faint: cssVar("--faint"),
    accent: cssVar("--accent"),
    panel: cssVar("--panel-solid"),
    font: cssVar("--font-sans"),
    mono: cssVar("--font-mono"),
  };
}

/** Changes when the theme does (chosen, or the system's), to repaint canvases. */
export function useThemeVersion(): number {
  const [version, setVersion] = useState(0);
  useEffect(() => {
    const bump = () => setVersion((value) => value + 1);
    const observer = new MutationObserver(bump);
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    media.addEventListener("change", bump);
    return () => {
      observer.disconnect();
      media.removeEventListener("change", bump);
    };
  }, []);
  return version;
}
