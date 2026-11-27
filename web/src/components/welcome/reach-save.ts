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
  /**
   * The reference of this zone and token's provider, the same on every
   * attempt (kept by digest across a reload, never the token): written into
   * the provider's description before it is created, so an attempt whose
   * response was lost, or one before a reload, is found instead of stored
   * twice.
   */
  attemptRef: (key: string) => Promise<string>
  /**
   * Whether an active provider with exactly this description exists. One
   * disabled since (by an administrator) is not used: a new one is made.
   */
  findDnsProvider: (description: string) => Promise<boolean>
  /**
   * Give the consent only where there is none (`create_only`): the server
   * refuses, changing nothing, when one was given meanwhile. Resolves to
   * when it was given.
   */
  servePrivately: (listen: string) => Promise<{ updated_at: string }>
  /**
   * This machine's private-ingress consent now (when it was given or last
   * changed), or `null` without one. One given before (in Settings, with
   * its own ranges) is kept as it is: writing the wizard's would replace
   * those ranges with the interface's whole subnet.
   */
  currentPrivateConsent: () => Promise<{ updated_at: string } | null>
  recordReach: (reach: DefaultReach) => Promise<unknown>
}

/** What earlier attempts of this save already wrote. */
export interface ReachSaveProgress {
  /** The provider created, by its name and token. */
  providerCreated?: string
  /** When the consent this save gave was given. */
  consentGiven?: string
}

/** Said when the private network the plan was made for is gone. */
export const NO_PRIVATE_NETWORK =
  'This box is on no private network it can serve on any more, so nothing was saved. Join one (Tailscale, WireGuard or your LAN) and plan again, or choose another way in.'

/** Said when the consent is not the one the plan was made from. */
export const CONSENT_CHANGED =
  'This machine’s private-ingress consent was given, changed or withdrawn after this plan was made, so nothing was saved. Reload the page to see the plan for the consent as it is now.'

/**
 * Whether a DNS provider this save created already exists: an active one
 * whose description carries its attempt reference. A disabled one does not
 * count, so the save creates a working one again.
 */
export function hasDnsProvider(
  providers: { is_active: boolean; description?: string | null }[],
  description: string
): boolean {
  return providers.some((p) => p.is_active && p.description === description)
}

export function providerKey(provider: DnsProviderBody): string {
  return `${provider.name}\n${provider.credentials.api_token}`
}

export async function saveReach(
  choice: AccessChoice,
  facts: Pick<BoxFacts, 'privateNetwork'>,
  steps: ReachSaveSteps,
  progress: ReachSaveProgress
): Promise<DefaultReach> {
  const reach = defaultReachFor(choice)
  const provider = dnsProviderFor(choice)
  const network = provider ? facts.privateNetwork : null
  // A private way in with no private network left would record a reach
  // nothing serves: refused before anything is written.
  if (provider && !network) throw new Error(NO_PRIVATE_NETWORK)
  if (network) {
    // Before anything is written: the plan shown (its listener, who may
    // connect) is about the consent it was made from, or about none. A
    // consent given, changed or withdrawn since is another plan, and a
    // refusal now leaves nothing to undo (a DNS provider stored twice).
    const made = network.consentAt ?? progress.consentGiven ?? null
    const now = await steps.currentPrivateConsent()
    if ((now?.updated_at ?? null) !== made) throw new Error(CONSENT_CHANGED)
  }
  if (choice.way === 'direct' && reach.naming === 'wildcard' && reach.domain) {
    await steps.setPreviewDomain(reach.domain)
  }
  if (provider && network) {
    // ADR-115: this machine serves the apps only on its private network,
    // and the record and the certificate need the provider. A consent
    // given before is kept as it is.
    if (!network.consentAt && !progress.consentGiven) {
      const given = await steps.servePrivately(network.listen)
      progress.consentGiven = given.updated_at
    }
    // Looked up on every attempt, even after this one created it: deleted
    // or disabled since, a retry must make a working one again rather than
    // finish the step without it.
    const key = providerKey(provider)
    const ref = await steps.attemptRef(key)
    const body = {
      ...provider,
      description: `${provider.description} (ref ${ref})`,
    }
    if (!(await steps.findDnsProvider(body.description))) {
      await steps.createDnsProvider(body)
    }
    progress.providerCreated = key
  }
  await steps.recordReach(reach)
  return reach
}

/**
 * A 64-bit FNV-1a digest of `text`, as hex: enough to tell one zone and token
 * from another in session storage, without keeping the token there. Not
 * `crypto.subtle`, which a console opened over plain HTTP on a LAN address
 * (an insecure context) does not have.
 */
function slotDigest(text: string): string {
  let hash = 0xcbf29ce484222325n
  for (const byte of new TextEncoder().encode(text)) {
    hash ^= BigInt(byte)
    hash = (hash * 0x100000001b3n) & 0xffffffffffffffffn
  }
  return hash.toString(16).padStart(16, '0')
}

/**
 * The attempt reference for `key` (a zone and its token), kept in `storage`
 * under a digest of the key so the token itself is never stored there. Made
 * once, before the first attempt, and the same after a reload in this tab.
 * Works in an insecure context: `crypto.getRandomValues` is there.
 */
export function sessionAttemptRef(
  key: string,
  storage: Pick<Storage, 'getItem' | 'setItem'> = window.sessionStorage
): Promise<string> {
  const slot = `temps.welcome.dnsProvider.${slotDigest(key)}`
  const kept = storage.getItem(slot)
  if (kept) return Promise.resolve(kept)
  const made = Array.from(crypto.getRandomValues(new Uint8Array(6)), (b) =>
    b.toString(16).padStart(2, '0')
  ).join('')
  storage.setItem(slot, made)
  return Promise.resolve(made)
}
