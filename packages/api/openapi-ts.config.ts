// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { defineConfig } from '@hey-api/openapi-ts'

// Generates the typed client into src/. Input is the checked-in openapi.json
// (kept in sync with the server's spec — regenerate after API changes via
// `bun run generate`). Mirrors the CLI's generator config so the output shape
// is identical, but this package is the single shared source of truth.
export default defineConfig({
  input: 'openapi.json',
  output: {
    path: 'src',
    format: 'prettier',
  },
  plugins: [
    '@hey-api/client-fetch',
    '@hey-api/sdk',
    '@hey-api/typescript',
  ],
})
