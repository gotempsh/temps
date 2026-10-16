// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  MAX_S3_BUCKETS,
  bucketEnvVarName,
  bucketNamesError,
  bucketsParameter,
  parseBucketInput,
} from './s3-buckets'

describe('parseBucketInput', () => {
  test('splits on commas, spaces and newlines, lower-cases and de-duplicates', () => {
    expect(parseBucketInput(' Uploads, media\nexports  uploads ,')).toEqual([
      'uploads',
      'media',
      'exports',
    ])
  })

  test('an empty field declares no buckets', () => {
    expect(parseBucketInput('  ,\n ')).toEqual([])
  })
})

describe('bucketNamesError', () => {
  test('accepts valid names', () => {
    expect(bucketNamesError(['uploads', 'media-2', 'a1b'])).toBeNull()
  })

  test('rejects names outside the length bounds', () => {
    expect(bucketNamesError(['ab'])).toContain('3-32')
    expect(bucketNamesError(['a'.repeat(33)])).toContain('3-32')
    expect(bucketNamesError(['a'.repeat(32)])).toBeNull()
  })

  test('rejects characters S3 buckets cannot carry', () => {
    expect(bucketNamesError(['my_bucket'])).toContain('lower-case')
    expect(bucketNamesError(['my.bucket'])).toContain('lower-case')
  })

  test('rejects names that start or end with a hyphen', () => {
    expect(bucketNamesError(['-media'])).toContain('start and end')
    expect(bucketNamesError(['media-'])).toContain('start and end')
  })

  test('caps the number of buckets', () => {
    const many = Array.from({ length: MAX_S3_BUCKETS + 1 }, (_, i) => `b-${i}x`)
    expect(bucketNamesError(many)).toContain(`${MAX_S3_BUCKETS}`)
    expect(bucketNamesError(many.slice(0, MAX_S3_BUCKETS))).toBeNull()
  })
})

describe('bucket parameters', () => {
  test('env var names are upper-snake-cased', () => {
    expect(bucketEnvVarName('uploads')).toBe('S3_BUCKET_UPLOADS')
    expect(bucketEnvVarName('user-avatars')).toBe('S3_BUCKET_USER_AVATARS')
  })

  test('the parameter is omitted when no buckets are declared', () => {
    expect(bucketsParameter([])).toBeUndefined()
    expect(bucketsParameter(['a-b', 'c-d'])).toBe('a-b,c-d')
  })
})
