// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  DEFAULT_GARAGE_IMAGE,
  DEFAULT_REDIS_IMAGE,
  DEFAULT_RUSTFS_IMAGE,
} from './service-images'
import { getSupportedImages } from '@/components/storage/UpgradeServiceDialog-shared'
import { REDIS_MANAGED_IMAGE } from '@/components/forms/ServiceTypePresets-shared'

describe('managed service image defaults', () => {
  test('uses the OTLP-capable RustFS release', () => {
    expect(DEFAULT_RUSTFS_IMAGE).toBe('rustfs/rustfs:1.0.0')
  })

  test('pins Garage to a release tag', () => {
    expect(DEFAULT_GARAGE_IMAGE).toBe('dxflrs/garage:v2.1.0')
    expect(DEFAULT_GARAGE_IMAGE.endsWith(':latest')).toBe(false)
  })

  test('creates and upgrades Redis to the weekly rebuilt trixie image', () => {
    expect(DEFAULT_REDIS_IMAGE).toBe('ghcr.io/gotempsh/redis-walg:8-trixie')
    expect(REDIS_MANAGED_IMAGE).toBe(DEFAULT_REDIS_IMAGE)
    expect(getSupportedImages('redis').map((option) => option.image)).toEqual([
      DEFAULT_REDIS_IMAGE,
    ])
  })
})
