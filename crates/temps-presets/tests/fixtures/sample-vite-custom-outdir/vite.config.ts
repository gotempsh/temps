// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { defineConfig } from 'vite'

// The bundle goes to `build/` instead of Vite's default `dist/`; the generated
// image must copy that directory into nginx.
export default defineConfig({
  server: { proxy: { '/api': 'http://localhost:3000' } },
  build: {
    outDir: 'build',
    emptyOutDir: true,
  },
})
