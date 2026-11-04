// Inline icons (24×24, stroked), so the Studio works offline.

const PATHS: Record<string, string> = {
  overview: "M3 3h7v9H3zM14 3h7v5h-7zM14 12h7v9h-7zM3 16h7v5H3z",
  explorer: "M4 6c0-1.7 3.6-3 8-3s8 1.3 8 3-3.6 3-8 3-8-1.3-8-3zM4 6v6c0 1.7 3.6 3 8 3s8-1.3 8-3V6M4 12v6c0 1.7 3.6 3 8 3s8-1.3 8-3v-6",
  graph: "M6 6m-2.5 0a2.5 2.5 0 1 0 5 0a2.5 2.5 0 1 0-5 0M18 8m-2.5 0a2.5 2.5 0 1 0 5 0a2.5 2.5 0 1 0-5 0M9 18m-2.5 0a2.5 2.5 0 1 0 5 0a2.5 2.5 0 1 0-5 0M8.3 7l7.3.7M7 8.3l1.4 7.3M16.3 9.8l-5.6 6.5",
  scripts: "M4 4h16v16H4zM8 9l3 3-3 3M13 15h3",
  search: "M11 11m-7 0a7 7 0 1 0 14 0a7 7 0 1 0-14 0M20 20l-4-4",
  memory: "M12 4a4 4 0 0 0-4 4v.3A3.5 3.5 0 0 0 6 15a3.5 3.5 0 0 0 6 2.5A3.5 3.5 0 0 0 18 15a3.5 3.5 0 0 0-2-6.7V8a4 4 0 0 0-4-4zM12 4v13.5",
  admin: "M12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6zM19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z",
  plus: "M12 5v14M5 12h14",
  node: "M12 12m-7 0a7 7 0 1 0 14 0a7 7 0 1 0-14 0M12 9v6M9 12h6",
  link: "M5 12m-2.5 0a2.5 2.5 0 1 0 5 0a2.5 2.5 0 1 0-5 0M19 12m-2.5 0a2.5 2.5 0 1 0 5 0a2.5 2.5 0 1 0-5 0M7.5 12h9M14 9l3 3-3 3",
  trash: "M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3",
  edit: "M4 20h4L19 9l-4-4L4 16zM14 6l4 4",
  play: "M7 4v16l13-8z",
  playAll: "M4 4v16l9-8zM13 4v16l9-8z",
  save: "M5 3h11l3 3v15H5zM8 3v5h7V3M8 21v-7h8v7",
  download: "M12 4v11M7 10l5 5 5-5M5 20h14",
  upload: "M12 20V9M7 14l5-5 5 5M5 4h14",
  x: "M6 6l12 12M18 6L6 18",
  check: "M5 12l5 5 9-10",
  sun: "M12 12m-4 0a4 4 0 1 0 8 0a4 4 0 1 0-8 0M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4",
  moon: "M20 14.5A8 8 0 0 1 9.5 4 8 8 0 1 0 20 14.5z",
  monitor: "M3 4h18v12H3zM8 20h8M12 16v4",
  archive: "M3 4h18v4H3zM5 8v12h14V8M10 12h4",
  history: "M3 12a9 9 0 1 0 3-6.7L3 8M3 3v5h5M12 7v5l3 3",
  file: "M6 2h9l5 5v15H6zM14 2v6h6",
  folder: "M3 6h6l2 2h10v11H3z",
  table: "M3 4h18v16H3zM3 10h18M3 15h18M9 4v16",
  braces: "M8 4c-2 0-3 1-3 3v2c0 1-1 2-2 3 1 1 2 2 2 3v2c0 2 1 3 3 3M16 4c2 0 3 1 3 3v2c0 1 1 2 2 3-1 1-2 2-2 3v2c0 2-1 3-3 3",
  command: "M9 6a3 3 0 1 0-3 3h12a3 3 0 1 0-3-3v12a3 3 0 1 0 3-3H6a3 3 0 1 0 3 3z",
  chevronLeft: "M15 6l-6 6 6 6",
  chevronRight: "M9 6l6 6-6 6",
  chevronDown: "M6 9l6 6 6-6",
  expand: "M15 3h6v6M9 21H3v-6M21 3l-7 7M3 21l7-7",
  fit: "M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5",
  eraser: "M7 21h10M5 15l9-9 5 5-9 9H8z",
  clock: "M12 12m-9 0a9 9 0 1 0 18 0a9 9 0 1 0-18 0M12 7v5l3 2",
  undo: "M9 14L4 9l5-5M4 9h11a5 5 0 0 1 0 10h-3",
  ban: "M12 12m-9 0a9 9 0 1 0 18 0a9 9 0 1 0-18 0M5.7 5.7l12.6 12.6",
  copy: "M8 8h12v12H8zM4 16V4h12",
  logout: "M9 21H4V3h5M16 17l5-5-5-5M21 12H9",
};

export type IconName = keyof typeof PATHS;

export function Icon({ name, size = 16, className }: { name: IconName; size?: number; className?: string }) {
  return (
    <svg
      className={className ? `icon ${className}` : "icon"}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d={PATHS[name]} />
    </svg>
  );
}

const BONDS =
  "M10.46 16.46L10.46 28.89M10.46 16.46L21.23 10.24M10.46 28.89L21.23 35.11M21.23 10.24L32 16.46M21.23 35.11L21.23 47.54M21.23 35.11L32 28.89M21.23 47.54L32 53.76M32 16.46L32 28.89M32 16.46L42.77 10.24M32 28.89L42.77 35.11M32 53.76L42.77 47.54M42.77 10.24L53.54 16.46M42.77 35.11L42.77 47.54M42.77 35.11L53.54 28.89M53.54 16.46L53.54 28.89";

const ATOMS: [number, number][] = [
  [10.46, 16.46], [10.46, 28.89], [21.23, 10.24], [21.23, 35.11], [21.23, 47.54], [32, 16.46],
  [32, 53.76], [42.77, 10.24], [42.77, 35.11], [42.77, 47.54], [53.54, 16.46], [53.54, 28.89],
];

/** Grafeno's mark: three fused carbon rings of a graphene lattice. */
export function Logo({ size = 28 }: { size?: number }) {
  return (
    <svg className="logo" width={size} height={size} viewBox="0 0 64 64" aria-hidden="true">
      <defs>
        <linearGradient id="logo-bond" x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stopColor="var(--logo-a)" />
          <stop offset="1" stopColor="var(--logo-b)" />
        </linearGradient>
      </defs>
      <path d={BONDS} fill="none" stroke="url(#logo-bond)" strokeWidth="4" strokeLinecap="round" />
      {ATOMS.map(([x, y]) => (
        <circle key={`${x}-${y}`} cx={x} cy={y} r="4.4" fill="var(--logo-atom)" />
      ))}
      <circle cx="32" cy="28.89" r="6" fill="var(--logo-a)" stroke="var(--logo-atom)" strokeWidth="2.2" />
    </svg>
  );
}
