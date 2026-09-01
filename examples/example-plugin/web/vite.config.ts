// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The plugin UI is served at /api/x/seo-analyzer/ui/
// In dev mode, vite runs standalone and proxies API calls to the plugin process.
export default defineConfig({
  plugins: [react()],
  base: "/api/x/seo-analyzer/ui/",
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
  server: {
    port: 5174,
    proxy: {
      // Proxy plugin API calls to the running plugin binary
      "/api/x/seo-analyzer": {
        target: "http://localhost:8081",
        changeOrigin: true,
      },
    },
  },
});
