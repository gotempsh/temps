// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { fileURLToPath } from 'node:url'
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      // @temps-sdk/ds and @temps-sdk/ui reach into web/src by relative path
      // (the same pattern web/src's own code uses internally via `@/*`) —
      // register the same alias here so the sandbox resolves it too.
      '@': fileURLToPath(new URL('../web/src', import.meta.url)),
      // Force every 'react'/'react-dom' import (ours, @temps-sdk/ds's,
      // @temps-sdk/ui's, react-router's) onto web's single installed copy.
      // Two React copies in one tree throws "Invalid hook call" at runtime,
      // not build time — this isn't optional.
      react: fileURLToPath(new URL('../web/node_modules/react', import.meta.url)),
      'react-dom': fileURLToPath(new URL('../web/node_modules/react-dom', import.meta.url)),
    },
  },
})
