// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import path from 'node:path'
import { defineConfig } from '@rsbuild/core'
import { pluginReact } from '@rsbuild/plugin-react'

const rsbuildOutputPath = process.env.RSBUILD_OUTPUT_PATH as string | undefined
const nodeEnv = process.env.NODE_ENV as string | undefined
const tempsVersion = process.env.TEMPS_VERSION || 'dev'
const apiTarget = process.env.TEMPS_API_TARGET || 'http://localhost:8080'
// The public proxy handles ordinary Console API requests, but it does not
// tunnel Console WebSocket upgrades. Keep the normal API path realistic while
// making the chat live-wire talk to the Console listener in development. Dev
// slots allocate the Console listener immediately after the public API port,
// so derive it from TEMPS_API_TARGET instead of silently falling back to slot
// zero whenever only the documented API override is supplied.
export const deriveConsoleTarget = (target: string) => {
  const url = new URL(target)
  const port = Number(url.port || (url.protocol === 'https:' ? 443 : 80))
  url.port = String(port + 1)
  url.pathname = ''
  url.search = ''
  url.hash = ''
  return url.origin
}
const consoleTarget =
  process.env.TEMPS_CONSOLE_TARGET || deriveConsoleTarget(apiTarget)
const isConversationLiveStream = (pathname: string) =>
  /^\/api\/(?:projects\/[^/]+\/ai\/conversations|ai\/conversations)\/[^/]+\/stream$/.test(
    pathname
  )
const consoleKitEntry = path.resolve(
  __dirname,
  'packages/console-kit/src/index.ts'
)

export default defineConfig({
  plugins: [pluginReact()],
  resolve: {
    alias: {
      // Local workspace package — pin explicitly so rsbuild resolves it even
      // when node_modules/@temps-sdk/console-kit is missing or stale.
      '@temps-sdk/console-kit': consoleKitEntry,
    },
  },
  source: {
    define: {
      'import.meta.env.TEMPS_VERSION': JSON.stringify(tempsVersion),
    },
  },
  html: {
    title: 'Temps',
    favicon: './src/favicon.png',
  },
  server: {
    // The live conversation stream is a Console WebSocket, whereas the
    // remaining /api surface belongs to the API listener. Use the native
    // http-proxy-middleware filter: its WebSocket upgrade handler evaluates
    // every configured proxy, so a broad unfiltered entry corrupts the socket
    // after the Console handler has accepted it.
    proxy: [
      {
        pathFilter: isConversationLiveStream,
        target: consoleTarget,
        // Preserve the browser Host header so the Console listener's
        // same-origin WebSocket guard sees the same authority as Origin.
        changeOrigin: false,
        ws: true,
      },
      {
        // Ordinary API requests are HTTP-only. Registering this broad proxy as
        // WebSocket-capable makes http-proxy-middleware attach a second upgrade
        // handler; depending on handler order it can consume/corrupt the chat
        // socket even though the path filter excludes `/stream`.
        pathFilter: (pathname) =>
          pathname.startsWith('/api') && !isConversationLiveStream(pathname),
        // Override to point the dev server at a different backend (e.g. the
        // dev-cluster control plane on :80): TEMPS_API_TARGET=http://localhost:80
        target: apiTarget,
        headers: {},
        changeOrigin: true,
        ws: false,
      },
    ],
    headers: {
      'Cache-Control': 'no-cache, no-store, must-revalidate',
      Pragma: 'no-cache',
      Expires: '0',
    },
  },
  output: {
    // Allow custom output path from environment variable (used by Rust build.rs)
    ...(rsbuildOutputPath && {
      distPath: {
        root: rsbuildOutputPath,
      },
    }),
    // Add contenthash to filenames for cache busting
    filename: {
      js: '[name].[contenthash:8].js',
      css: '[name].[contenthash:8].css',
    },
    // Disable caching in development
    ...(nodeEnv === 'development' && {
      filename: {
        js: '[name].js?v=[hash:8]',
        css: '[name].css?v=[hash:8]',
      },
    }),
  },
  dev: {
    lazyCompilation: false,
  },
})
