// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { defineConfig } from '@hey-api/openapi-ts'

export default defineConfig({
  input: 'openapi.json',
  // input: 'http://localhost:3000/api-docs/openapi.json',
  output: 'src/api',
  plugins: [
    '@hey-api/client-fetch',
    '@hey-api/sdk',
    '@hey-api/typescript',
  ],
})
