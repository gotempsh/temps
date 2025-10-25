import { defineConfig } from '@rsbuild/core'
import { pluginReact } from '@rsbuild/plugin-react'
const rsbuildOutputPath = process.env.RSBUILD_OUTPUT_PATH as string | undefined
const nodeEnv = process.env.NODE_ENV as string | undefined
export default defineConfig({
  plugins: [pluginReact()],
  html: {
    title: 'Temps',
    favicon: './src/favicon.png',
  },
  server: {
    proxy: {
      '/api': {
        target: 'http://localhost:8081',
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
    lazyCompilation: false, // Add headers to prevent caching in development
    headers: {
      'Cache-Control': 'no-cache, no-store, must-revalidate',
      Pragma: 'no-cache',
      Expires: '0',
    },
  },
})
