// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { describeActivity } from './identical-builds'

test('recent activity is described in words, and nothing when idle', () => {
  expect(
    describeActivity({ reused: 0, built: 0, waiting: 0, building: 0 })
  ).toBeNull()
  expect(
    describeActivity({ reused: 2, built: 1, waiting: 1, building: 0 })
  ).toBe(
    "Last 30 days: 2 deployments reused an identical build's image, 1 coordinated build ran, 1 waiting now."
  )
})
