// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * How the console reads the refusals the instance-wide proxy settings give
 * (custom routes and the IP block list). The server answers each with an
 * RFC 7807 problem whose `title` names the case, so the form can put the
 * message next to the field it is about, or ask for the confirmation the
 * server is waiting for, instead of showing one generic failure toast.
 */

import { extractProblemDetails } from '@/utils/errorHandling'

/** A refused custom route, tied to the field the operator has to change. */
export interface RouteFieldProblem {
  field: 'domain' | 'host'
  message: string
}

const ROUTE_DOMAIN_TITLES = new Set(['Invalid Route Domain', 'Domain In Use'])
const ROUTE_UPSTREAM_TITLE = 'Invalid Route Upstream'

/**
 * The form field a refused route save is about, with the server's explanation.
 * `null` for anything else (a 403, a 5xx, a network error): those are not
 * about one field and keep the generic failure toast.
 */
export function routeFieldProblem(error: unknown): RouteFieldProblem | null {
  const problem = extractProblemDetails(error)
  if (!problem) return null
  const message = problem.detail || problem.title
  if (ROUTE_DOMAIN_TITLES.has(problem.title)) {
    return { field: 'domain', message }
  }
  if (problem.title === ROUTE_UPSTREAM_TITLE) {
    return { field: 'host', message }
  }
  return null
}

/** What an IP block rule save is waiting for the operator to confirm. */
export type IpRuleConfirmation = 'broad_range' | 'lockout'

const BROAD_RANGE_TITLE = 'Block Range Needs Confirmation'
const LOCKOUT_TITLE = 'Rule Would Lock You Out'

/**
 * Whether a refused IP rule save is one the operator can confirm. Other
 * conflicts (a duplicate address) are 409 too, so the title decides.
 */
export function ipRuleConfirmation(error: unknown): IpRuleConfirmation | null {
  const problem = extractProblemDetails(error)
  if (!problem || problem.status !== 409) return null
  if (problem.title === BROAD_RANGE_TITLE) return 'broad_range'
  if (problem.title === LOCKOUT_TITLE) return 'lockout'
  return null
}

/** The request flag that answers a confirmation. */
export function confirmationFlag(
  confirmation: IpRuleConfirmation
): 'confirm_broad_range' | 'acknowledge_lockout' {
  return confirmation === 'broad_range'
    ? 'confirm_broad_range'
    : 'acknowledge_lockout'
}
