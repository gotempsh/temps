// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// React Query surface over `/projects/{project_id}/edge-rules` (ADR-089 §9).
// Components only use these hooks; `api.ts` converts the API's rule format
// to the editor's. A saved rule is live on every proxy within seconds.

import {
  createEdgeRule,
  deleteEdgeRule,
  listEdgeRules,
  reorderEdgeRules,
  updateEdgeRule,
} from '@/api/client'
import {
  queryOptions,
  useMutation,
  useQueryClient,
} from '@tanstack/react-query'
import { draftToApi, ruleFromApi, type ApiRule } from './api'
import type { EdgeRule, EdgeRuleDraft } from './types'

/** The generated client types `expression`/`action` loosely; `api.ts` owns
 * their shape. */
const fromApi = (rule: unknown): EdgeRule => ruleFromApi(rule as ApiRule)

const ruleId = (projectId: number, id: string): number => {
  const parsed = Number(id)
  if (!Number.isInteger(parsed) || parsed <= 0) {
    throw new Error(`Edge rule id "${id}" in project ${projectId} is invalid.`)
  }
  return parsed
}

const repository = {
  list: async (projectId: number): Promise<EdgeRule[]> => {
    const { data } = await listEdgeRules({
      path: { project_id: projectId },
      throwOnError: true,
    })
    return data.rules.map(fromApi)
  },

  create: async (projectId: number, draft: EdgeRuleDraft) => {
    const { data } = await createEdgeRule({
      path: { project_id: projectId },
      body: draftToApi(draft) as never,
      throwOnError: true,
    })
    return fromApi(data)
  },

  /** Sends the revision that was edited; a stale one is refused with 409. */
  update: async (
    projectId: number,
    id: string,
    revision: number,
    patch: Partial<EdgeRuleDraft>
  ) => {
    const body: Record<string, unknown> = { revision }
    if (patch.name !== undefined) body.name = patch.name.trim()
    if (patch.enabled !== undefined) body.enabled = patch.enabled
    if (patch.mode !== undefined) body.mode = patch.mode
    if (patch.environmentId !== undefined) {
      body.environment_id = patch.environmentId
    }
    if (patch.expression !== undefined || patch.action !== undefined) {
      const full = draftToApi({
        name: '',
        enabled: true,
        mode: 'enforce',
        environmentId: null,
        expression: patch.expression ?? {
          kind: 'group',
          id: 'unused',
          combinator: 'all',
          children: [],
        },
        action: patch.action ?? { type: 'log' },
      })
      if (patch.expression !== undefined) body.expression = full.expression
      if (patch.action !== undefined) body.action = full.action
    }
    const { data } = await updateEdgeRule({
      path: { project_id: projectId, rule_id: ruleId(projectId, id) },
      body: body as never,
      throwOnError: true,
    })
    return fromApi(data)
  },

  remove: async (projectId: number, id: string) => {
    await deleteEdgeRule({
      path: { project_id: projectId, rule_id: ruleId(projectId, id) },
      throwOnError: true,
    })
  },

  /** Moves a rule to `toIndex`, shifting the others, as one ordered write. */
  move: async (
    projectId: number,
    current: EdgeRule[],
    id: string,
    toIndex: number
  ) => {
    const ids = current.map((r) => r.id)
    const from = ids.indexOf(id)
    if (from === -1) {
      throw new Error(`Edge rule ${id} was not found in project ${projectId}.`)
    }
    ids.splice(from, 1)
    ids.splice(Math.max(0, Math.min(toIndex, ids.length)), 0, id)
    const { data } = await reorderEdgeRules({
      path: { project_id: projectId },
      body: { rule_ids: ids.map((r) => ruleId(projectId, r)) },
      throwOnError: true,
    })
    return data.rules.map(fromApi)
  },
}

export const edgeRulesQueryKey = (projectId: number) =>
  ['edge-rules', projectId] as const

export const edgeRulesOptions = (projectId: number) =>
  queryOptions({
    queryKey: edgeRulesQueryKey(projectId),
    queryFn: () => repository.list(projectId),
    // Hit counters move while the page is open.
    refetchInterval: 30_000,
  })

function useInvalidate(projectId: number) {
  const queryClient = useQueryClient()
  return () =>
    queryClient.invalidateQueries({ queryKey: edgeRulesQueryKey(projectId) })
}

export function useCreateEdgeRule(projectId: number) {
  const invalidate = useInvalidate(projectId)
  return useMutation({
    mutationFn: (draft: EdgeRuleDraft) => repository.create(projectId, draft),
    meta: { errorTitle: 'Failed to create edge rule' },
    onSuccess: invalidate,
  })
}

export function useUpdateEdgeRule(projectId: number) {
  const invalidate = useInvalidate(projectId)
  return useMutation({
    mutationFn: (input: {
      ruleId: string
      revision: number
      patch: Partial<EdgeRuleDraft>
    }) =>
      repository.update(projectId, input.ruleId, input.revision, input.patch),
    meta: { errorTitle: 'Failed to update edge rule' },
    // Also after a 409, so the list shows the revision that won.
    onSettled: invalidate,
  })
}

export function useDeleteEdgeRule(projectId: number) {
  const invalidate = useInvalidate(projectId)
  return useMutation({
    mutationFn: (ruleId: string) => repository.remove(projectId, ruleId),
    meta: { errorTitle: 'Failed to delete edge rule' },
    onSettled: invalidate,
  })
}

export function useMoveEdgeRule(projectId: number) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (input: { ruleId: string; toIndex: number }) =>
      repository.move(
        projectId,
        queryClient.getQueryData<EdgeRule[]>(edgeRulesQueryKey(projectId)) ??
          [],
        input.ruleId,
        input.toIndex
      ),
    meta: { errorTitle: 'Failed to reorder edge rules' },
    onSuccess: (rules) =>
      queryClient.setQueryData(edgeRulesQueryKey(projectId), rules),
    onError: () =>
      queryClient.invalidateQueries({
        queryKey: edgeRulesQueryKey(projectId),
      }),
  })
}
