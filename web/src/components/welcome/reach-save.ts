// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Saving the access step: up to four writes in a row (the preview domain, a
 * DNS provider with the pasted token, the private-ingress consent, the
 * onboarding record). Any of them can fail after an earlier one landed, and
 * the person then saves again. `progress` remembers the provider already
 * created for this zone and token, so a retry never stores the token twice;
 * the other writes are idempotent and simply run again.
 */

import {
  defaultReachFor,
  dnsProviderFor,
  type AccessChoice,
  type BoxFacts,
} from './access'

export type DefaultReach = ReturnType<typeof defaultReachFor>
export type DnsProviderBody = NonNullable<ReturnType<typeof dnsProviderFor>>

export interface ReachSaveSteps {
  setPreviewDomain: (domain: string) => Promise<unknown>
  createDnsProvider: (body: DnsProviderBody) => Promise<unknown>
  servePrivately: (listen: string) => Promise<unknown>
  recordReach: (reach: DefaultReach) => Promise<unknown>
}

/** What earlier attempts of this save already wrote. */
export interface ReachSaveProgress {
  /** The provider created, by its name and token. */
  providerCreated?: string
}

function providerKey(provider: DnsProviderBody): string {
  return `${provider.name}\n${provider.credentials.api_token}`
}

export async function saveReach(
  choice: AccessChoice,
  facts: Pick<BoxFacts, 'privateNetwork'>,
  steps: ReachSaveSteps,
  progress: ReachSaveProgress
): Promise<DefaultReach> {
  const reach = defaultReachFor(choice)
  if (choice.way === 'direct' && reach.naming === 'wildcard' && reach.domain) {
    await steps.setPreviewDomain(reach.domain)
  }
  const provider = dnsProviderFor(choice)
  if (provider && facts.privateNetwork) {
    // ADR-115: the record and the certificate need the provider, and this
    // machine serves the apps only on its private network.
    const key = providerKey(provider)
    if (progress.providerCreated !== key) {
      await steps.createDnsProvider(provider)
      progress.providerCreated = key
    }
    await steps.servePrivately(facts.privateNetwork.listen)
  }
  await steps.recordReach(reach)
  return reach
}
