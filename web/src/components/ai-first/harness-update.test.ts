// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  harnessUpdateDetail,
  harnessUpdateHeadline,
  harnessUpdateRefetchInterval,
  harnessVersionChange,
  shouldShowHarnessUpdate,
} from './harness-update'

const available = {
  installed_version: '2.1.263',
  latest_version: '2.1.296',
  update_available: true,
  updating: false,
}

describe('harness update bar', () => {
  test('is shown only for an available or running update', () => {
    expect(shouldShowHarnessUpdate(undefined)).toBe(false)
    expect(
      shouldShowHarnessUpdate({ ...available, update_available: false })
    ).toBe(false)
    expect(shouldShowHarnessUpdate(available)).toBe(true)
    expect(
      shouldShowHarnessUpdate({
        ...available,
        update_available: false,
        updating: true,
      })
    ).toBe(true)
  })

  test('names the version change and explains why updating matters', () => {
    expect(harnessVersionChange(available)).toBe('2.1.263 → 2.1.296')
    expect(
      harnessVersionChange({ ...available, installed_version: null })
    ).toBe('2.1.296')
    expect(harnessUpdateHeadline('Claude Code', available)).toBe(
      'Claude Code update available'
    )
    expect(harnessUpdateDetail('Claude Code', available)).toContain(
      "Models released after your installed version won't be offered"
    )
  })

  test('says it is updating and polls quickly meanwhile', () => {
    const running = { ...available, updating: true }
    expect(harnessUpdateHeadline('Codex', running)).toBe('Updating Codex…')
    expect(harnessUpdateDetail('Codex', running)).toContain('Installing')
    expect(harnessUpdateRefetchInterval(running)).toBe(3_000)
    expect(harnessUpdateRefetchInterval(available)).toBeGreaterThan(60_000)
    expect(harnessUpdateRefetchInterval(undefined)).toBeGreaterThan(60_000)
  })
})
