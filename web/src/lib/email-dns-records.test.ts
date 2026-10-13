// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import { isAdvisoryDnsRecord } from './email-dns-records'

describe('isAdvisoryDnsRecord', () => {
  it('treats MX and DMARC as advisory', () => {
    expect(
      isAdvisoryDnsRecord({ record_type: 'MX', name: 'example.com' })
    ).toBe(true)
    expect(
      isAdvisoryDnsRecord({ record_type: 'TXT', name: '_dmarc.example.com' })
    ).toBe(true)
  })

  it('treats SPF and DKIM records as required', () => {
    expect(
      isAdvisoryDnsRecord({ record_type: 'TXT', name: 'example.com' })
    ).toBe(false)
    expect(
      isAdvisoryDnsRecord({
        record_type: 'CNAME',
        name: 'abc._domainkey.example.com',
      })
    ).toBe(false)
  })
})
