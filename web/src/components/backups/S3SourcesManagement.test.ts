// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import { shouldShowS3SourceHeaderAction } from '@/lib/s3-source-presentation'

describe('shouldShowS3SourceHeaderAction', () => {
  test('avoids duplicating the create action in the empty state', () => {
    expect(shouldShowS3SourceHeaderAction(false, 0)).toBe(false)
    expect(shouldShowS3SourceHeaderAction(true, 0)).toBe(false)
    expect(shouldShowS3SourceHeaderAction(false, 1)).toBe(true)
  })
})
