// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'

import { describeProtection } from './cluster-health'

test('a cluster whose first standby is still copying says it is not yet protected', () => {
  expect(
    describeProtection({
      state: 'not_protected',
      detail:
        'Not yet protected: standby app-ha-2 (catchingup) is still copying or catching up with the primary.',
    })
  ).toEqual({
    tone: 'not-protected',
    title: 'Not yet protected',
    body: 'Standby app-ha-2 (catchingup) is still copying or catching up with the primary.',
  })
})

test('a cluster without a usable standby is not protected', () => {
  expect(
    describeProtection({
      state: 'not_protected',
      detail: 'Not protected: the cluster has no standby',
    })
  ).toEqual({
    tone: 'not-protected',
    title: 'Not protected',
    body: 'The cluster has no standby',
  })
})

test('protected and unknown map to their own tones', () => {
  expect(
    describeProtection({ state: 'protected', detail: 'Protected: standby x' })
      .tone
  ).toBe('protected')
  expect(
    describeProtection({ state: 'unknown', detail: 'monitor unreachable' }).tone
  ).toBe('unknown')
})
