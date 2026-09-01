// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react()],
  base: "/api/x/deployment-pulse/ui/",
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
});
