// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { aiTracesSetupHref } from './ai-traces-setup'

test('configure telemetry opens the selected service traces setup', () => {
  expect(aiTracesSetupHref('shop-api')).toBe(
    '/projects/shop-api/traces#traces-setup'
  )
})

test('without a service it goes to the service list, not the gateway', () => {
  expect(aiTracesSetupHref(undefined)).toBe('/projects')
  expect(aiTracesSetupHref(null)).toBe('/projects')
})
