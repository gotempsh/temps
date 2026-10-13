// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { EmailDomainWithDnsResponse } from '@/api/client'

type DnsRecord = EmailDomainWithDnsResponse['dns_records'][number]

/**
 * MX and DMARC are advisory: the backend's are_all_records_verified gate
 * excludes them, so a domain can be "verified" while they are still unset.
 * MX is a deliverability aid rather than a sending/auth prerequisite, and
 * DMARC is a plain TXT record the domain owner sets independently (no
 * provider API manages it).
 */
export function isAdvisoryDnsRecord(
  record: Pick<DnsRecord, 'record_type' | 'name'>
): boolean {
  return record.record_type === 'MX' || record.name.startsWith('_dmarc.')
}
