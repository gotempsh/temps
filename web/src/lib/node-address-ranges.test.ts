// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { meshRangeToShow } from './node-address-ranges'

describe('meshRangeToShow', () => {
  test('shows the range nodes have their addresses from once the mesh is ready', () => {
    expect(meshRangeToShow({ state: 'ready', cidr: '10.201.0.0/16' })).toEqual({
      cidr: '10.201.0.0/16',
      ready: true,
    })
  })

  test('shows it as not ready yet while the mesh starts', () => {
    expect(
      meshRangeToShow({ state: 'starting', cidr: '10.201.0.0/16' })
    ).toEqual({ cidr: '10.201.0.0/16', ready: false })
  })

  test('shows nothing while the mesh is off or unreadable', () => {
    expect(
      meshRangeToShow({ state: 'disabled', cidr: '10.201.0.0/16' })
    ).toBeUndefined()
    expect(meshRangeToShow({ state: 'ready', cidr: null })).toBeUndefined()
    expect(meshRangeToShow(undefined)).toBeUndefined()
  })
})
