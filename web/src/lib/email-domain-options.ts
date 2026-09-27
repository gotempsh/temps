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

export interface ProviderScopedSelection {
  domain: string
  providerIdentityId: string
}

/**
 * The domain/identity selection to keep when the provider changes.
 *
 * A provider identity ID belongs to one provider account, and when importing,
 * the domain was picked together with it. Keeping either across a provider
 * switch would submit provider A's identity under provider B, creating an
 * import that can never verify or send. In create mode the domain is free
 * text and independent of the provider, so only the identity is dropped.
 * Choosing the first provider keeps a domain typed before any provider was
 * selected: nothing was picked from a provider yet, so nothing is stale.
 */
export function selectionAfterProviderChange(
  mode: 'create' | 'import',
  previousProviderId: number | undefined,
  nextProviderId: number,
  selection: ProviderScopedSelection
): ProviderScopedSelection {
  if (previousProviderId === nextProviderId) return selection
  return {
    domain:
      mode === 'import' && previousProviderId !== undefined
        ? ''
        : selection.domain,
    providerIdentityId: '',
  }
}
