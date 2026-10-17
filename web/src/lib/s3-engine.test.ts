// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { s3EngineLabel, s3MigrationSupported } from './s3-engine'

describe('s3EngineLabel', () => {
  test('services without a backend run RustFS', () => {
    expect(s3EngineLabel('s3', {})).toBe('RustFS')
    expect(s3EngineLabel('s3', null)).toBe('RustFS')
    expect(s3EngineLabel('blob', undefined)).toBe('RustFS')
  })

  test('reads the selected backend', () => {
    expect(s3EngineLabel('s3', { backend: 'garage' })).toBe('Garage')
    expect(s3EngineLabel('s3', { backend: ' Garage ' })).toBe('Garage')
    expect(s3EngineLabel('s3', { backend: 'rustfs' })).toBe('RustFS')
  })

  test('legacy MinIO stays labelled as such', () => {
    expect(s3EngineLabel('minio', {})).toBe('MinIO (legacy)')
    expect(s3EngineLabel('s3', { backend: 'minio' })).toBe('MinIO (legacy)')
  })

  test('other service types have no engine', () => {
    expect(s3EngineLabel('postgres', { backend: 'garage' })).toBeNull()
  })
})

describe('s3MigrationSupported', () => {
  test('RustFS and Garage can be migrated; MinIO and databases cannot', () => {
    expect(s3MigrationSupported('s3', {})).toBe(true)
    expect(s3MigrationSupported('s3', { backend: 'garage' })).toBe(true)
    expect(s3MigrationSupported('minio', {})).toBe(false)
    expect(s3MigrationSupported('redis', {})).toBe(false)
  })
})
