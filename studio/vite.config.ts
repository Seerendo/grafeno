import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// `npm run dev` talks to a local `grafeno serve` through a proxy;
// GRAFENO_URL picks another server than the default address.
declare const process: { env: Record<string, string | undefined> };
const server = process.env.GRAFENO_URL ?? "http://127.0.0.1:7810";

// Relative asset paths: the same build works at /studio/ (embedded in
// `grafeno serve`) or at any other location.
export default defineConfig({
  base: "./",
  plugins: [react()],
  server: {
    fs: { allow: [".."] },
    proxy: { "/v1": server, "/health": server },
  },
  build: { outDir: "dist", emptyOutDir: true, chunkSizeWarningLimit: 1500 },
});
