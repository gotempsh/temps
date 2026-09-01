// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  base: "/api/x/google-indexing/ui/",
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
  server: {
    port: 5176,
    proxy: {
      "/api/x/google-indexing": {
        target: "http://localhost:8081",
        changeOrigin: true,
      },
    },
  },
});
