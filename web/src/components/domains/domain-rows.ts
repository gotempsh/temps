// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// One list of domains, however they get HTTPS (ADR 058 D10): certificates
// from `/domains`, and names a tunnel carries from `/ingress/*`. Certificates
// are paginated and sorted by the server; tunnel names are few, so they are
// fetched whole, filtered here and merged into the first page.

import type {
  ConnectorView,
  DomainResponse,
  HostnameView,
} from '@/api/client/types.gen'
import type { DomainSort } from './domain-sort'

export type DomainRow =
  | { kind: 'certificate'; key: string; name: string; domain: DomainResponse }
  | { kind: 'tunnel'; key: string; name: string; view: HostnameView }
  | { kind: 'tunnel_zone'; key: string; name: string; connector: ConnectorView }

/** Where a row's detail page is. */
export function domainRowHref(row: DomainRow): string {
  return row.kind === 'certificate'
    ? `/domains/${row.domain.id}`
    : `/domains/tunnel/${encodeURIComponent(row.name)}`
}

/** Status order for sorting tunnel rows among certificates: trouble first. */
const TUNNEL_STATUS_RANK: Record<string, number> = {
  error: 0,
  pending: 1,
  not_published: 2,
  published: 3,
}

export function tunnelRows(
  connectors: ConnectorView[],
  hostnames: HostnameView[],
  search: string
): DomainRow[] {
  const needle = search.trim().toLowerCase()
  const zones: DomainRow[] = connectors.map((connector) => ({
    kind: 'tunnel_zone',
    key: `zone:${connector.id}`,
    name: `*.${connector.domain}`,
    connector,
  }))
  const names: DomainRow[] = hostnames.map((view) => ({
    kind: 'tunnel',
    key: `tunnel:${view.hostname}`,
    name: view.hostname,
    view,
  }))
  return [...zones, ...names].filter(
    (row) => !needle || row.name.includes(needle)
  )
}

/**
 * The page's rows: certificates as the server sorted them, with tunnel rows
 * merged in on the first page. Sorting by name interleaves them; by status,
 * failing tunnel names come first; by expiry, they go last (the provider
 * renews their certificates).
 */
export function mergeDomainRows(
  certificates: DomainResponse[],
  tunnels: DomainRow[],
  page: number,
  sort: DomainSort,
  direction: 'asc' | 'desc'
): DomainRow[] {
  const certRows: DomainRow[] = certificates.map((domain) => ({
    kind: 'certificate',
    key: `cert:${domain.id}`,
    name: domain.domain,
    domain,
  }))
  // A name with both a certificate and a tunnel shows once, as the
  // certificate: that is the one that needs attention.
  const certNames = new Set(certificates.map((d) => d.domain))
  const extra =
    page === 1 ? tunnels.filter((row) => !certNames.has(row.name)) : []
  if (extra.length === 0) return certRows

  const flip = direction === 'desc' ? -1 : 1
  if (sort === 'domain') {
    return [...certRows, ...extra].sort(
      (a, b) => flip * a.name.localeCompare(b.name)
    )
  }
  if (sort === 'status') {
    const rank = (row: DomainRow) =>
      row.kind === 'tunnel' ? (TUNNEL_STATUS_RANK[row.view.state] ?? 2) : 3
    const sortedExtra = [...extra].sort((a, b) => rank(a) - rank(b))
    const troubled = sortedExtra.filter((row) => rank(row) < 2)
    return [...troubled, ...certRows, ...sortedExtra.filter((r) => rank(r) >= 2)]
  }
  return [...certRows, ...extra]
}
