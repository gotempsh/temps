// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export interface DiscoverableDomain {
  domain: string
  provider_identity_id: string
  status: string
}

/**
 * Picker options for the domains a provider reports, keyed by provider
 * identity ID rather than name. When two identities share a name, the label
 * adds the status and a short identity ID so the rows can be told apart.
 */
export function discoverableDomainOptions(
  domains: DiscoverableDomain[]
): { value: string; label: string; keywords: string }[] {
  const nameCounts = new Map<string, number>()
  for (const d of domains)
    nameCounts.set(d.domain, (nameCounts.get(d.domain) ?? 0) + 1)
  return domains.map((d) => ({
    value: d.provider_identity_id,
    label:
      (nameCounts.get(d.domain) ?? 0) > 1
        ? `${d.domain} (${d.status.replace(/_/g, ' ')}, ${d.provider_identity_id.slice(0, 8)})`
        : d.domain,
    keywords: d.status,
  }))
}
