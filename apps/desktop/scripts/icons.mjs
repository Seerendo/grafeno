// Renders every icon of Grafeno from one source, docs/assets/logo.svg:
// the desktop app's icons (Tauri), the Windows icon of grafeno.exe and its
// installer, and the Linux icon theme files of the server packages.
//
//   npm run icons
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, readdirSync, rmSync } from "node:fs";
import { join } from "node:path";

const here = join(import.meta.dirname, "..");
const root = join(here, "..", "..");
const icons = join(here, "icons");

execFileSync("npx", ["tauri", "icon", join(root, "docs", "assets", "logo.svg"), "--output", icons], {
  cwd: here,
  stdio: "inherit",
  shell: process.platform === "win32",
});

// Only desktop icons: no mobile or Windows Store tiles.
const keep = new Set(["32x32.png", "64x64.png", "128x128.png", "128x128@2x.png", "icon.png", "icon.ico", "icon.icns"]);
for (const name of readdirSync(icons)) {
  if (!keep.has(name)) rmSync(join(icons, name), { recursive: true, force: true });
}

copyFileSync(join(icons, "icon.ico"), join(root, "packaging", "windows", "grafeno.ico"));
const theme = join(root, "packaging", "linux", "icons");
mkdirSync(theme, { recursive: true });
for (const [file, size] of [["32x32.png", 32], ["64x64.png", 64], ["128x128.png", 128], ["128x128@2x.png", 256], ["icon.png", 512]]) {
  copyFileSync(join(icons, file), join(theme, `grafeno-${size}.png`));
}
console.log("icons written");
