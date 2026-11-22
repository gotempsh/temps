// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The one tab holding every deployment the chat watched, newest first. A
 * deployment's own key (`project:deployment`) still works as a selection, e.g.
 * from the address: it opens this tab on that deployment.
 */
export const DEPLOYMENTS_TAB = 'deployments'

/**
 * Which tab a selection opens and, on the Deployments tab, which deployment it
 * shows. A selection that is no longer offered falls back to `fallback`; the
 * Deployments tab without a pick shows the newest deployment.
 */
export function sidebarSelection(
  selected: string,
  {
    tabs,
    deploymentKeys,
    fallback,
  }: {
    tabs: readonly string[]
    deploymentKeys: readonly string[]
    fallback: string
  }
): { tab: string; deployment: string | null } {
  const newest = deploymentKeys[0] ?? null
  if (deploymentKeys.includes(selected)) {
    return { tab: DEPLOYMENTS_TAB, deployment: selected }
  }
  if (selected === DEPLOYMENTS_TAB && newest) {
    return { tab: DEPLOYMENTS_TAB, deployment: newest }
  }
  return {
    tab: tabs.includes(selected) ? selected : fallback,
    deployment: newest,
  }
}
