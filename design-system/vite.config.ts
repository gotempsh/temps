// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { fileURLToPath, URL } from 'node:url'
import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
      // The op primitives live in the @temps-sdk/op workspace package under
      // ../web. The sandbox consumes them by source (no build step, HMR
      // still works) rather than declaring a dependency, so design-system's
      // package.json stays untouched.
      // More specific first: object aliases are prefix matches, so
      // '@temps-sdk/op' would otherwise swallow the '/op.css' subpath.
      '@temps-sdk/op/op.css': fileURLToPath(
        new URL('../web/packages/op/src/op.css', import.meta.url)
      ),
      '@temps-sdk/op': fileURLToPath(
        new URL('../web/packages/op/src/index.ts', import.meta.url)
      ),
    },
    // The aliased package sits inside ../web, so Vite would happily resolve
    // its bare `react` import against web/node_modules and load a SECOND
    // React — which throws "Invalid hook call" the moment an op component
    // uses a hook. Dedupe pins every consumer to the sandbox's copy.
    dedupe: ['react', 'react-dom', 'react-router'],
  },
  server: {
    port: 5183,
    // Bind to all interfaces (not just loopback) so the sandbox can be opened
    // from another machine on the network. Add that machine's hostname to
    // `allowedHosts` if Vite's Host-header check (DNS-rebinding protection)
    // rejects it.
    host: true,
  },
})
