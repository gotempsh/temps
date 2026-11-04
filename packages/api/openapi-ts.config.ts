// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { defineConfig } from '@hey-api/openapi-ts'

// Generates the typed client into src/. Input is the canonical spec committed
// at apps/temps-cli/openapi.json, the only copy in the repository (regenerate
// after API changes via `bun run generate`). Mirrors the CLI's generator config
// so the output shape is identical.
export default defineConfig({
  input: '../../apps/temps-cli/openapi.json',
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
