// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Deployment states the server accepts as a rollback target.
 *
 * Mirrors `valid_rollback_states` in the deployments service's
 * `rollback_to_deployment_as`; anything else is refused with
 * "Cannot rollback to deployment in '<state>' state". `stopped` is the state a
 * successful deployment ends up in once a later one supersedes it, so it is
 * the primary rollback case. `failed`, `cancelled`, `paused`, `pending`,
 * `running` and `building` have no reliable image to reuse and are rejected.
 */
export const ROLLBACK_TARGET_STATUSES: readonly string[] = [
  'deployed',
  'completed',
  'stopped',
]

/**
 * Whether "Rollback to this deployment" should be offered for a deployment.
 *
 * The deployment that is already serving traffic is never a rollback target:
 * rolling back to it would only redeploy what is live (use Redeploy for that).
 */
export function canRollbackTo(deployment: {
  status: string
  is_current?: boolean
}): boolean {
  return (
    !deployment.is_current &&
    ROLLBACK_TARGET_STATUSES.includes(deployment.status)
  )
}

/** Statuses that always have an entry in the detail page's "More actions" menu. */
const DETAIL_MENU_STATUSES: readonly string[] = [
  'completed',
  'paused',
  'running',
  'pending',
]

/**
 * Whether the deployment detail page's "More actions" menu has anything to
 * show. Rendering the menu for a deployment with no items would leave an
 * empty popover, so this must stay in step with the menu's entries.
 */
export function deploymentMenuHasActions(deployment: {
  status: string
  is_current?: boolean
}): boolean {
  return (
    canRollbackTo(deployment) ||
    DETAIL_MENU_STATUSES.includes(deployment.status)
  )
}
