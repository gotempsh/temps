// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Link } from 'react-router'

/** How the managed-domain read resolved, as the route form sees it. */
export type ManagedDomainsStatus = 'pending' | 'error' | 'empty' | 'available'

/**
 * The line under the Domain field of the Add Route form. It tells a failed or
 * still-running read of the managed domains apart from "none exist", and in
 * the latter case links to where one is added instead of just saying so.
 */
export function DomainFieldHint({
  isLocalMode,
  status,
  inputType,
}: {
  isLocalMode: boolean
  status: ManagedDomainsStatus
  inputType: 'select' | 'manual'
}) {
  if (isLocalMode) {
    return <>Manual entry required in local development mode</>
  }
  switch (status) {
    case 'pending':
      return <>Checking your managed domains…</>
    case 'error':
      return (
        <>
          Your managed domains could not be loaded. Enter the domain manually,
          or reload the page to try again.
        </>
      )
    case 'empty':
      return (
        <>
          No managed domains yet, so enter the domain manually. To pick one from
          a list with a certificate,{' '}
          <Link className="underline underline-offset-4" to="/domains/add">
            add a domain
          </Link>{' '}
          first.
        </>
      )
    case 'available':
      return inputType === 'select' ? (
        <>Choose from your managed domains with SSL certificates</>
      ) : (
        <>Enter the full domain name for this route</>
      )
  }
}
