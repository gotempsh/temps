// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import type { ApplicationResponse } from '@/api/client'
import {
  applicationsFromPages,
  nextApplicationPage,
  resolveApplicationSelection,
} from './application-list'

function application(publicId: string): ApplicationResponse {
  return { public_id: publicId } as ApplicationResponse
}

describe('applicationsFromPages', () => {
  it('uses refetched infinite-query pages as the authoritative list', () => {
    const before = applicationsFromPages([
      [application('app-1'), application('app-2')],
      [application('app-3')],
    ])
    expect(before.map((item) => item.public_id)).toEqual([
      'app-1',
      'app-2',
      'app-3',
    ])

    const after = applicationsFromPages([
      [application('app-new'), application('app-1')],
      [application('app-2')],
    ])
    expect(after.map((item) => item.public_id)).toEqual([
      'app-new',
      'app-1',
      'app-2',
    ])
    expect(after.some((item) => item.public_id === 'app-3')).toBe(false)
  })

  it('deduplicates a deep-linked application already present in a page', () => {
    const selected = application('app-2')
    expect(
      applicationsFromPages([[application('app-1'), selected]], selected).map(
        (item) => item.public_id
      )
    ).toEqual(['app-1', 'app-2'])
  })

  it('keeps pagination reachable when a new deep-linked application is selected over a full page', () => {
    const fullPage = Array.from({ length: 50 }, (_, index) =>
      application(`app-${index}`)
    )
    const selected = application('app-new')

    expect(applicationsFromPages([fullPage], selected)).toHaveLength(51)
    expect(fullPage).toHaveLength(50)
    expect(nextApplicationPage(fullPage, 1, 50)).toBe(2)
  })
})

describe('resolveApplicationSelection', () => {
  it('lets browser navigation replace a still-valid current selection', () => {
    const applications = [application('app-a'), application('app-b')]
    expect(
      resolveApplicationSelection(applications, 'app-b', 'app-a', false)
    ).toBe('app-b')
  })

  it('uses the global scope before either application selection', () => {
    expect(
      resolveApplicationSelection(
        [application('app-a')],
        'app-a',
        'app-a',
        true
      )
    ).toBeNull()
  })
})
