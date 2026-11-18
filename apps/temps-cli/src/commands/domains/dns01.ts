// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// DNS-01 for `temps domains add -c dns-01`: when a DNS provider in Temps
// manages the domain's zone, the server publishes the `_acme-challenge` TXT
// record itself and the CLI waits for it, then finalizes the order. Manual
// TXT instructions are only the fallback when no provider manages the zone.

import { Resolver } from 'node:dns/promises'
import type { DnsProviderResponse, ManagedDomainResponse } from '../../api/types.gen.js'

/** A provider and the zones it manages, as the API lists them. */
export interface ProviderZones {
  provider: Pick<DnsProviderResponse, 'id' | 'name' | 'is_active'>
  zones: Array<Pick<ManagedDomainResponse, 'domain' | 'verified' | 'auto_manage'>>
  /** Why this provider's zones could not be listed; its `zones` are then unknown, not empty. */
  listingError?: string
}

export type Dns01Route =
  | { mode: 'provider'; providerId: number; providerName: string; zone: string }
  | { mode: 'manual'; reason: string }

function normalize(name: string): string {
  return name.trim().replace(/\.$/, '').toLowerCase()
}

/**
 * Which way the DNS-01 challenge for `domain` is answered. Mirrors the
 * server's choice of provider for a domain: the most specific zone that
 * covers it, verified and auto-managed, of an active provider. Two rows for
 * that zone are ambiguous, so the CLI falls back to manual instructions
 * rather than guess.
 */
export function chooseDns01Route(domain: string, candidates: ProviderZones[]): Dns01Route {
  const host = normalize(domain.startsWith('*.') ? domain.slice(2) : domain)
  // A provider whose zones could not be listed may manage this domain too:
  // choosing among the rest could pick the wrong provider, or miss that two
  // claim the zone. Fall back to manual instructions rather than guess.
  const unlisted = candidates.find((c) => c.provider.is_active && c.listingError !== undefined)
  if (unlisted) {
    return {
      mode: 'manual',
      reason: `Could not list the zones of DNS provider ${unlisted.provider.name} (${unlisted.listingError}), so Temps cannot tell which provider manages ${host}.`,
    }
  }
  const covering: Array<{ providerId: number; providerName: string; zone: string }> = []
  for (const { provider, zones } of candidates) {
    if (!provider.is_active) continue
    for (const zone of zones) {
      const name = normalize(zone.domain)
      if (!zone.verified || !zone.auto_manage || !name) continue
      if (host === name || host.endsWith(`.${name}`)) {
        covering.push({ providerId: provider.id, providerName: provider.name, zone: name })
      }
    }
  }
  if (covering.length === 0) {
    return {
      mode: 'manual',
      reason: `No DNS provider in Temps manages a verified zone for ${host} with automatic DNS on.`,
    }
  }
  const longest = Math.max(...covering.map((c) => c.zone.length))
  const best = covering.filter((c) => c.zone.length === longest)
  if (best.length > 1) {
    return {
      mode: 'manual',
      reason: `More than one DNS provider manages ${best[0]!.zone}, so Temps cannot tell which one to use.`,
    }
  }
  const [only] = best
  return { mode: 'provider', ...only! }
}

/** Public resolvers asked whether the challenge record is visible yet. */
const PUBLIC_RESOLVERS = ['1.1.1.1', '8.8.8.8']

export type TxtLookup = (name: string) => Promise<string[]>

/** TXT values at `name` as public resolvers see them; empty when none. */
export function publicTxtLookup(servers: string[] = PUBLIC_RESOLVERS): TxtLookup {
  return async (name) => {
    const resolver = new Resolver({ timeout: 5000, tries: 1 })
    resolver.setServers(servers)
    try {
      const records = await resolver.resolveTxt(name)
      return records.map((chunks) => chunks.join(''))
    } catch {
      return []
    }
  }
}

/**
 * Wait until every expected value is published at `name`. ACME validates
 * once: finalizing before the record is visible invalidates the order, so
 * the CLI only finalizes after this returns true.
 */
export async function waitForTxt(
  name: string,
  expected: string[],
  options: { lookup?: TxtLookup; timeoutMs?: number; intervalMs?: number; now?: () => number; sleep?: (ms: number) => Promise<void> } = {},
): Promise<boolean> {
  const lookup = options.lookup ?? publicTxtLookup()
  const timeoutMs = options.timeoutMs ?? 180_000
  const intervalMs = options.intervalMs ?? 5_000
  const now = options.now ?? Date.now
  const sleep = options.sleep ?? ((ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms)))
  const wanted = expected.filter((v) => v.length > 0)
  if (wanted.length === 0) return false
  const deadline = now() + timeoutMs
  for (;;) {
    const seen = await lookup(name)
    if (wanted.every((value) => seen.includes(value))) return true
    if (now() >= deadline) return false
    await sleep(intervalMs)
  }
}
