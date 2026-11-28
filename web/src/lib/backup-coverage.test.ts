// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  NOTHING_TO_BACK_UP_MESSAGE,
  coverageError,
  coverageModeOf,
  coverageRequest,
  coverageSummary,
} from './backup-coverage'

describe('coverageRequest', () => {
  test('control plane only saves no databases and forces the control plane on', () => {
    expect(coverageRequest('control_plane', [3, 4], false)).toEqual({
      target_all_services: false,
      include_control_plane: true,
      service_ids: [],
    })
  })

  test('all databases ignores the picked list', () => {
    expect(coverageRequest('all', [3], true)).toEqual({
      target_all_services: true,
      include_control_plane: true,
      service_ids: [],
    })
  })

  test('specific databases keep the picked list and the switch', () => {
    expect(coverageRequest('specific', [3], false)).toEqual({
      target_all_services: false,
      include_control_plane: false,
      service_ids: [3],
    })
  })
})

describe('coverageError', () => {
  test('only a schedule that backs up nothing is refused', () => {
    expect(coverageError('specific', [], false)).toBe(
      NOTHING_TO_BACK_UP_MESSAGE
    )
    expect(coverageError('specific', [], true)).toBeUndefined()
    expect(coverageError('specific', [1], false)).toBeUndefined()
    expect(coverageError('control_plane', [], false)).toBeUndefined()
    expect(coverageError('all', [], false)).toBeUndefined()
  })
})

describe('coverageModeOf', () => {
  test('a specific schedule with no databases and the control plane is control plane only', () => {
    expect(
      coverageModeOf(
        { target_all_services: false, include_control_plane: true },
        0
      )
    ).toBe('control_plane')
  })

  test('databases attached or the control plane off stay specific', () => {
    expect(
      coverageModeOf(
        { target_all_services: false, include_control_plane: true },
        2
      )
    ).toBe('specific')
    expect(
      coverageModeOf(
        { target_all_services: false, include_control_plane: false },
        0
      )
    ).toBe('specific')
  })

  test('all databases wins over the attached list', () => {
    expect(
      coverageModeOf(
        { target_all_services: true, include_control_plane: false },
        0
      )
    ).toBe('all')
  })
})

describe('coverageSummary', () => {
  test('describes a control-plane-only schedule instead of zero databases', () => {
    expect(
      coverageSummary(
        { target_all_services: false, include_control_plane: true },
        0
      )
    ).toBe('Only the Temps control plane')
  })

  test('counts databases and mentions the control plane', () => {
    expect(
      coverageSummary(
        { target_all_services: false, include_control_plane: true },
        1
      )
    ).toBe('1 database and the Temps control plane')
    expect(
      coverageSummary(
        { target_all_services: false, include_control_plane: false },
        3
      )
    ).toBe('3 databases')
    expect(
      coverageSummary(
        { target_all_services: true, include_control_plane: true },
        0
      )
    ).toBe('All databases and the Temps control plane')
  })

  test('says plainly when a schedule backs up nothing', () => {
    expect(
      coverageSummary(
        { target_all_services: false, include_control_plane: false },
        0
      )
    ).toMatch(/^Nothing/)
  })
})
