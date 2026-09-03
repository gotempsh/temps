// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { DEFAULT_RUSTFS_IMAGE } from './service-images'

describe('managed service image defaults', () => {
  test('uses the OTLP-capable RustFS release', () => {
    expect(DEFAULT_RUSTFS_IMAGE).toBe('rustfs/rustfs:1.0.0-rc.5')
  })
})
