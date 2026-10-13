// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  selectCustomDomainLookup,
  selectDomainOrder,
  selectLastDeployment,
} from './empty-state-selectors'

describe('empty-state selectors', () => {
  test('a project without deployments selects null, not undefined', () => {
    expect(selectLastDeployment({ deployment: null })).toBeNull()
    expect(selectLastDeployment({})).toBeNull()
  })

  test('a deployment is passed through untouched', () => {
    const deployment = { id: 7, status: 'completed' }
    expect(selectLastDeployment({ deployment })).toBe(deployment)
  })

  test('an unattached hostname selects null', () => {
    expect(selectCustomDomainLookup({ custom_domain: null })).toBeNull()
  })

  test('an attached hostname is passed through untouched', () => {
    const custom_domain = { id: 3, project_id: 9 }
    expect(selectCustomDomainLookup({ custom_domain })).toBe(custom_domain)
  })

  test('a domain without an order selects null and ignores the reason', () => {
    expect(
      selectDomainOrder({ order: null, reason: 'certificate is active' })
    ).toBeNull()
  })

  test('an existing order is passed through untouched', () => {
    const order = { id: 5, status: 'pending' }
    expect(selectDomainOrder({ order })).toBe(order)
  })
})
