// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Which tunnels can carry a domain (ADR 058 D10). A tunnel publishes names
// under its own domain; the wizard and the domain picker use this to offer
// "through the tunnel" instead of a certificate.

import { queryOptions } from '@tanstack/react-query'
import { listIngressConnectorsQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { listIngressConnectors } from '@/api/client/sdk.gen'
import type { ConnectorView } from '@/api/client/types.gen'

const TUNNEL_PAGE_SIZE = 100
/** The control plane's `MAX_CONNECTORS`: it refuses to create more. */
export const MAX_TUNNELS = 500

/**
 * Every tunnel, for screens that match names against all of them: worker
 * settings, domain choices and a tunnel's own page. The list is paginated,
 * so this reads page after page until it has them all, never more than
 * `MAX_TUNNELS`. The key extends the list's own, so invalidating the list
 * refreshes this too.
 */
export function allTunnelsOptions() {
  return queryOptions({
    queryKey: [
      {
        ...listIngressConnectorsQueryKey({
          query: { page_size: TUNNEL_PAGE_SIZE },
        })[0],
        allPages: true,
      },
    ],
    queryFn: async ({ signal }) => {
      const tunnels: ConnectorView[] = []
      for (let page = 1; page <= MAX_TUNNELS / TUNNEL_PAGE_SIZE; page++) {
        const { data } = await listIngressConnectors({
          query: { page, page_size: TUNNEL_PAGE_SIZE },
          signal,
          throwOnError: true,
        })
        if (data.total > MAX_TUNNELS) {
          throw new Error(
            `This cluster has ${data.total} tunnels, more than the ${MAX_TUNNELS} the console can match names against. Delete tunnels you no longer use from their worker's page in Settings.`
          )
        }
        tunnels.push(...data.connectors)
        if (data.connectors.length === 0 || tunnels.length >= data.total) {
          break
        }
      }
      return tunnels
    },
  })
}

/** `*.Example.com.` → `example.com`; `App.example.com` → `app.example.com`. */
export function baseHost(host: string): string {
  return host.trim().toLowerCase().replace(/\.$/, '').replace(/^\*\./, '')
}

/**
 * Tunnels whose domain covers `host` (an exact name or `*.domain`), the most
 * specific first and the oldest on a tie: the order the control plane picks
 * them in.
 */
export function tunnelsCovering(
  host: string,
  connectors: ConnectorView[]
): ConnectorView[] {
  const base = baseHost(host)
  if (!base) return []
  return connectors
    .filter((c) => {
      const domain = baseHost(c.domain)
      return base === domain || base.endsWith(`.${domain}`)
    })
    .sort((a, b) => b.domain.length - a.domain.length || a.id - b.id)
}

/**
 * How many labels the names under `host` sit below the tunnel's domain. The
 * tunnel's domain is usually the zone, and Cloudflare's free certificate only
 * covers one level below it.
 */
export function levelsBelow(host: string, connector: ConnectorView): number {
  const base = baseHost(host)
  const domain = baseHost(connector.domain)
  const own =
    base === domain ? 0 : base.slice(0, -domain.length - 1).split('.').length
  return host.trim().startsWith('*.') ? own + 1 : own
}

export const TUNNEL_PREFIX = 'tunnel:'

export function tunnelChoice(connectorId: number): string {
  return `${TUNNEL_PREFIX}${connectorId}`
}

export function chosenTunnel(value: string | undefined): number | null {
  if (!value?.startsWith(TUNNEL_PREFIX)) return null
  const id = Number(value.slice(TUNNEL_PREFIX.length))
  return Number.isInteger(id) ? id : null
}
