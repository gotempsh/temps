// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { NodePlacementReference } from '@/api/client'

/** Placements listed by name before the rest are counted. */
const LISTED_PLACEMENTS = 5

function placementName(placement: NodePlacementReference): string {
  return placement.environment_name
    ? `${placement.project_name} / ${placement.environment_name}`
    : `${placement.project_name} (project default)`
}

function nodeList(ids: number[]): string {
  return ids.map((id) => (id === 0 ? 'control plane' : `node ${id}`)).join(', ')
}

/**
 * One sentence for the "Node removed" toast naming the environments whose
 * target nodes listed the removed node, and what they target now. `null`
 * when no placement named it.
 */
export function describeUpdatedPlacements(
  placements: NodePlacementReference[] | undefined
): string | null {
  if (!placements || placements.length === 0) return null
  const listed = placements
    .slice(0, LISTED_PLACEMENTS)
    .map(
      (placement) =>
        `${placementName(placement)} now targets ${nodeList(placement.target_nodes)}`
    )
  const more = placements.length - listed.length
  return (
    `Removed from the target nodes of ${placements.length} ${
      placements.length === 1 ? 'environment' : 'environments'
    }: ${listed.join('; ')}${more > 0 ? `; and ${more} more` : ''}. ` +
    'A machine that rejoins gets a new node ID; add it to these environments if they should use it.'
  )
}
