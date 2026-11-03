import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Relative asset paths: the same build works at /studio/ (embedded in
// `grafeno serve`) or at any other location.
export default defineConfig({
  base: "./",
  plugins: [react()],
  server: {
    fs: { allow: [".."] },
    // `npm run dev` talks to a local `grafeno serve` through this proxy.
    proxy: {
      "/v1": "http://127.0.0.1:7810",
      "/health": "http://127.0.0.1:7810",
    },
  },
  build: { outDir: "dist", emptyOutDir: true, chunkSizeWarningLimit: 1500 },
});
