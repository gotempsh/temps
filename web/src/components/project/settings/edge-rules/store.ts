// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// UI PREVIEW STORAGE. The edge rules API (ADR-052 §9) does not exist yet, so
// rules are kept in this browser's localStorage behind the same React Query
// surface the generated client will provide. Replace the repository functions
// with the generated `/projects/{project_id}/edge-rules` calls when the
// backend lands; the components only use the hooks below.

import {
  queryOptions,
  useMutation,
  useQueryClient,
} from '@tanstack/react-query'
import { LIMITS } from './fields'
import type { EdgeRule, EdgeRuleDraft } from './types'

const storageKey = (projectId: number) =>
  `temps.edge-rules.preview.v1.${projectId}`

export class EdgeRuleConflictError extends Error {
  constructor(ruleId: string, expected: number, actual: number) {
    super(
      `Edge rule ${ruleId} was changed elsewhere (revision ${actual}, you edited ${expected}). Reload it and apply your change again.`
    )
    this.name = 'EdgeRuleConflictError'
  }
}

export class EdgeRuleNotFoundError extends Error {
  constructor(projectId: number, ruleId: string) {
    super(`Edge rule ${ruleId} was not found in project ${projectId}.`)
    this.name = 'EdgeRuleNotFoundError'
  }
}

function read(projectId: number): EdgeRule[] {
  const raw = window.localStorage.getItem(storageKey(projectId))
  if (!raw) return []
  try {
    const parsed: unknown = JSON.parse(raw)
    return Array.isArray(parsed) ? (parsed as EdgeRule[]) : []
  } catch {
    return []
  }
}

function write(projectId: number, rules: EdgeRule[]): void {
  window.localStorage.setItem(storageKey(projectId), JSON.stringify(rules))
}

/** Simulates request latency so loading and pending states are visible. */
const settle = <T>(value: () => T, ms = 180): Promise<T> =>
  new Promise((resolve, reject) =>
    setTimeout(() => {
      try {
        resolve(value())
      } catch (e) {
        reject(e)
      }
    }, ms)
  )

const newRuleId = () =>
  `rul_${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`

const repository = {
  list: (projectId: number) => settle(() => read(projectId)),

  create: (projectId: number, draft: EdgeRuleDraft) =>
    settle(() => {
      const rules = read(projectId)
      if (rules.length >= LIMITS.rulesPerProject) {
        throw new Error(
          `Project ${projectId} already has ${LIMITS.rulesPerProject} edge rules, the maximum.`
        )
      }
      const now = new Date().toISOString()
      const rule: EdgeRule = {
        ...draft,
        id: newRuleId(),
        revision: 1,
        createdAt: now,
        updatedAt: now,
      }
      write(projectId, [...rules, rule])
      return rule
    }),

  update: (
    projectId: number,
    ruleId: string,
    revision: number,
    patch: Partial<EdgeRuleDraft>
  ) =>
    settle(() => {
      const rules = read(projectId)
      const index = rules.findIndex((r) => r.id === ruleId)
      if (index === -1) throw new EdgeRuleNotFoundError(projectId, ruleId)
      const current = rules[index]
      if (current.revision !== revision) {
        throw new EdgeRuleConflictError(ruleId, revision, current.revision)
      }
      const updated: EdgeRule = {
        ...current,
        ...patch,
        revision: current.revision + 1,
        updatedAt: new Date().toISOString(),
      }
      rules[index] = updated
      write(projectId, rules)
      return updated
    }),

  remove: (projectId: number, ruleId: string) =>
    settle(() => {
      const rules = read(projectId)
      if (!rules.some((r) => r.id === ruleId)) {
        throw new EdgeRuleNotFoundError(projectId, ruleId)
      }
      write(
        projectId,
        rules.filter((r) => r.id !== ruleId)
      )
    }),

  /** Moves a rule to `toIndex`, shifting the others. */
  move: (projectId: number, ruleId: string, toIndex: number) =>
    settle(() => {
      const rules = read(projectId)
      const from = rules.findIndex((r) => r.id === ruleId)
      if (from === -1) throw new EdgeRuleNotFoundError(projectId, ruleId)
      const [rule] = rules.splice(from, 1)
      rules.splice(Math.max(0, Math.min(toIndex, rules.length)), 0, rule)
      write(projectId, rules)
      return rules
    }, 80),
}

export const edgeRulesQueryKey = (projectId: number) =>
  ['edge-rules', projectId] as const

export const edgeRulesOptions = (projectId: number) =>
  queryOptions({
    queryKey: edgeRulesQueryKey(projectId),
    queryFn: () => repository.list(projectId),
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
      repository.move(projectId, input.ruleId, input.toIndex),
    meta: { errorTitle: 'Failed to reorder edge rules' },
    onSuccess: (rules) =>
      queryClient.setQueryData(edgeRulesQueryKey(projectId), rules),
  })
}
