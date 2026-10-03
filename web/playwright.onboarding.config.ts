// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { defineConfig } from '@playwright/test'
export default defineConfig({
  testDir: './demo-tests',
  testMatch: ['projects-first-run.spec.ts', 'monitoring-project.spec.ts'],
  timeout: 30000,
  expect: { timeout: 10000 },
  use: {
    baseURL: process.env.PLAYWRIGHT_BASE_URL ?? 'http://localhost:3000',
    browserName: 'chromium',
  },
  reporter: 'list',
})
