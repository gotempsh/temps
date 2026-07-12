import path from 'node:path'
import { defineConfig } from '@rsbuild/core'
import { pluginReact } from '@rsbuild/plugin-react'

const rsbuildOutputPath = process.env.RSBUILD_OUTPUT_PATH as string | undefined
const nodeEnv = process.env.NODE_ENV as string | undefined
const tempsVersion = process.env.TEMPS_VERSION || 'dev'
const consoleKitEntry = path.resolve(__dirname, 'packages/console-kit/src/index.ts')

export default defineConfig({
  plugins: [pluginReact()],
  // Rsbuild v2 no longer auto-populates `performance.chunkSplit` on the
  // resolved config (it's deprecated in favor of `splitChunks`, but still
  // supported). @rsbuild/plugin-react@1.4.0 reads
  // `environment.config.performance.chunkSplit.strategy` without a null
  // check, so leaving this unset crashes `rsbuild build` with
  // "Cannot read properties of undefined (reading 'strategy')". Setting it
  // explicitly reproduces rsbuild v1's default strategy byte-for-byte, so
  // there's no behavior change — just an explicit opt-in the plugin needs.
  // Safe to drop once @rsbuild/plugin-react is bumped to a v2-compatible
  // release (tracked separately).
  performance: {
    chunkSplit: { strategy: 'split-by-experience' },
  },
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
    proxy: {
      '/api': {
        // Override to point the dev server at a different backend (e.g. the
        // dev-cluster control plane on :80): TEMPS_API_TARGET=http://localhost:80
        target: process.env.TEMPS_API_TARGET || 'http://localhost:8080',
        headers: {},
        changeOrigin: true,
        ws: true,
      },
    },
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
