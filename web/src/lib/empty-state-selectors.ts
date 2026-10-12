// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Selectors for the "nothing yet" answers of the API.
//
// A project with no deployments, a hostname that is not attached to a project
// and a domain without an ACME order are normal states, so the API answers 200
// with an explicit `null` instead of a 404. These selectors turn those wrappers
// into the value pages actually consume. They return `null` (not `undefined`)
// for "known to be empty" so callers can tell it apart from "still loading".

/** `GET /projects/{id}/last-deployment` -> the deployment, or `null` when none. */
export function selectLastDeployment<T>(response: {
  deployment?: T | null
}): T | null {
  return response.deployment ?? null
}

/** `GET /projects/custom-domains/by-host/{hostname}` -> the assignment, or `null`. */
export function selectCustomDomainLookup<T>(response: {
  custom_domain?: T | null
}): T | null {
  return response.custom_domain ?? null
}

/** `GET /domains/{id}/order` -> the ACME order, or `null` when there is none. */
export function selectDomainOrder<T>(response: { order?: T | null }): T | null {
  return response.order ?? null
}
