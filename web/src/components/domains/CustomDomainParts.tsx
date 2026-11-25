// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// The pieces a custom domain adds to the address it is: what it does besides
// serving the app (redirect, service, a conflict) and its tunnel, shown once
// in the Domains page's address list.

import { ExternalLink } from 'lucide-react'
import type { ReactNode } from 'react'
import type { CustomDomainResponse } from '@/api/client'
import { HostnameTunnelProblem } from '@/components/domains/TunnelHostnames'
import { addressUrl } from '@/lib/reachability'

/** Builds a domain's actions menu; supplied by the page that owns the dialogs. */
export type DomainMenu = (domain: CustomDomainResponse) => ReactNode

/** What a custom domain says besides its reachability, one line each. */
export function CustomDomainNotes({
  domain,
}: {
  domain: CustomDomainResponse
}) {
  return (
    <>
      {domain.status === 'hostname_conflict' && domain.message && (
        <p className="text-destructive">{domain.message}</p>
      )}
      {domain.redirect_to && (
        <p className="text-muted-foreground">
          Redirects to {domain.redirect_to}
          {domain.status_code ? ` (${domain.status_code})` : ''}
        </p>
      )}
      {domain.service_name && (
        <p className="text-muted-foreground">Serves {domain.service_name}</p>
      )}
      <HostnameTunnelProblem hostname={domain.domain} />
    </>
  )
}

export function AddressLink({ address }: { address: string }) {
  return (
    <a
      href={addressUrl(address)}
      target="_blank"
      rel="noreferrer"
      className="inline-flex min-w-0 items-center gap-1 font-medium [overflow-wrap:anywhere] underline-offset-4 hover:underline"
    >
      {address}
      <ExternalLink aria-hidden="true" className="h-3 w-3 shrink-0" />
    </a>
  )
}

/** A custom domain with no reachability to show beside it. */
export function CustomDomainRow({
  domain,
  menu,
}: {
  domain: CustomDomainResponse
  menu?: DomainMenu
}) {
  return (
    <li className="flex items-start gap-3 px-3 py-2.5 text-sm">
      <div className="min-w-0 flex-1 space-y-1">
        <AddressLink address={domain.domain} />
        <CustomDomainNotes domain={domain} />
      </div>
      {menu?.(domain)}
    </li>
  )
}
