// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  shouldShowS3SourceHeaderAction,
  workerCredentialsPresentation,
} from '@/lib/s3-source-presentation'

describe('shouldShowS3SourceHeaderAction', () => {
  test('avoids duplicating the create action in the empty state', () => {
    expect(shouldShowS3SourceHeaderAction(false, 0)).toBe(false)
    expect(shouldShowS3SourceHeaderAction(true, 0)).toBe(false)
    expect(shouldShowS3SourceHeaderAction(false, 1)).toBe(true)
  })
})

describe('workerCredentialsPresentation', () => {
  test('a scoped source refuses rather than sending its key', () => {
    const scoped = workerCredentialsPresentation('scoped', false)
    expect(scoped.warning).toBe(false)
    expect(scoped.label).toBe('Scoped per job')
    expect(scoped.description).toContain('refused')
    expect(workerCredentialsPresentation(undefined, false)).toEqual(scoped)
  })

  test('allowing full credentials is shown as a warning that names the risk', () => {
    const full = workerCredentialsPresentation('allow_full', false)
    expect(full.warning).toBe(true)
    expect(full.description).toContain('delete every backup')
  })

  test('a managed source is always scoped by Temps Cloud', () => {
    const managed = workerCredentialsPresentation('allow_full', true)
    expect(managed.warning).toBe(false)
    expect(managed.label).toBe('Scoped by Temps Cloud')
  })
})
