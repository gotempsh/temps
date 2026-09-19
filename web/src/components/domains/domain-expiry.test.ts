// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { hasUrgentCertificate } from './domain-expiry'

test('only serving certificates show expiry warnings', () => {
  for (const status of ['active', 'active_renewal_failed']) {
    expect(hasUrgentCertificate(status, 100, 200)).toBe(true)
    expect(hasUrgentCertificate(status, 300, 200)).toBe(true)
    expect(hasUrgentCertificate(status, 20 * 86400000, 200)).toBe(false)
  }
  for (const status of ['pending', 'pending_dns', 'failed']) {
    expect(hasUrgentCertificate(status, 100, 200)).toBe(false)
    expect(hasUrgentCertificate(status, 300, 200)).toBe(false)
  }
})
test('unknown expiry never produces a warning', () => {
  for (const value of [null, undefined, NaN])
    expect(hasUrgentCertificate('active', value)).toBe(false)
})
