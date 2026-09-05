import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

/**
 * The API is proxied in dev so the browser sees one origin: no CORS, and no
 * base-URL configuration needed to run the pilot locally.
 *
 * ponytail: the target is a constant rather than an env var, which would drag
 * in @types/node for a dev-only config. Change the line if the API moves.
 */
const API_TARGET = "http://127.0.0.1:8080";

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      "/api": { target: API_TARGET, changeOrigin: true },
    },
  },
});
