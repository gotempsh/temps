// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'
import type { SandboxPlacementResponse } from '@/api/client'

/** Node id the placement API uses for the control plane. */
export const CONTROL_PLANE_NODE_ID = 0

export const sandboxPlacementFormSchema = z.object({
  /** `true` = every node, including nodes added later (`allowed_node_ids: null`). */
  allowAll: z.boolean(),
  /** Explicit allow-list; ignored while `allowAll` is set. */
  selected: z.array(z.number().int().nonnegative()),
})

export type SandboxPlacementFormValues = z.infer<
  typeof sandboxPlacementFormSchema
>

/**
 * Form values for the saved placement. Only ids that still have a checkbox
 * are kept: a removed node must not linger invisibly in the selection and
 * fail the next save.
 */
export function placementFormValues(
  data: SandboxPlacementResponse | undefined
): SandboxPlacementFormValues {
  if (!data) return { allowAll: true, selected: [] }
  const allowed = data.allowed_node_ids ?? null
  const known = data.nodes.map((n) => n.id)
  return {
    allowAll: allowed === null,
    selected: sortedIds((allowed ?? known).filter((id) => known.includes(id))),
  }
}

/** The `allowed_node_ids` member to send: always explicit, `null` = every node. */
export function allowedNodeIdsFromForm(
  values: SandboxPlacementFormValues
): number[] | null {
  return values.allowAll ? null : sortedIds(values.selected)
}

/** Whether saving `values` would change the saved allow-list. */
export function placementFormDirty(
  data: SandboxPlacementResponse | undefined,
  values: SandboxPlacementFormValues
): boolean {
  if (!data) return false
  const saved = data.allowed_node_ids ?? null
  const next = allowedNodeIdsFromForm(values)
  if (saved === null || next === null) return saved !== next
  return sortedIds(saved).join(',') !== next.join(',')
}

/**
 * The control plane would not take new sandboxes, while some other node
 * would. (With nothing selected, nobody can create sandboxes, which has its
 * own message.)
 */
export function placementExcludesControlPlane(
  values: SandboxPlacementFormValues
): boolean {
  return (
    !values.allowAll &&
    values.selected.length > 0 &&
    !values.selected.includes(CONTROL_PLANE_NODE_ID)
  )
}

export function sortedIds(ids: readonly number[]): number[] {
  return [...new Set(ids)].sort((a, b) => a - b)
}
