// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { BuildNodePolicyResponse, NodeInfoResponse } from '@/api/client'
import { z } from 'zod'

export const buildNodeFormSchema = z
  .object({
    mode: z.enum(['default', 'custom']),
    ids: z.array(z.number().int().positive().max(2147483647)).max(100),
  })
  .superRefine((value, ctx) => {
    if (value.mode === 'custom' && value.ids.length === 0) {
      ctx.addIssue({
        code: 'custom',
        path: ['ids'],
        message: 'Select at least one worker, or use the default selection.',
      })
    }
    if (new Set(value.ids).size !== value.ids.length) {
      ctx.addIssue({
        code: 'custom',
        path: ['ids'],
        message: 'Each worker can appear only once.',
      })
    }
  })

export type BuildNodeFormValues = z.infer<typeof buildNodeFormSchema>

export function buildNodeDefaults(
  policy: BuildNodePolicyResponse
): BuildNodeFormValues {
  return {
    mode: policy.node_ids == null ? 'default' : 'custom',
    ids: policy.node_ids ?? [],
  }
}

export function buildNodeRequest(values: BuildNodeFormValues) {
  return { node_ids: values.mode === 'default' ? null : values.ids }
}

export function moveBuilder(
  ids: number[],
  index: number,
  direction: -1 | 1
): number[] {
  const destination = index + direction
  if (
    index < 0 ||
    index >= ids.length ||
    destination < 0 ||
    destination >= ids.length
  )
    return ids
  const next = [...ids]
  ;[next[index], next[destination]] = [next[destination], next[index]]
  return next
}

export function builderName(id: number, nodes: NodeInfoResponse[]): string {
  return nodes.find((node) => node.id === id)?.name ?? `Worker #${id}`
}

export function buildNodeError(error: unknown): string {
  if (
    error &&
    typeof error === 'object' &&
    'detail' in error &&
    typeof error.detail === 'string'
  )
    return error.detail
  return 'The request failed. Check your connection and permissions, then retry. Your selection has not been discarded.'
}
