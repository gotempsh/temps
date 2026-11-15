// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Editing a blueprint by hand. The server validates every saved version; these
 * helpers keep a draft in a shape it accepts: each part carries only the
 * fields of its kind, ids are slugs, and removing a part removes its flows.
 */

import type {
  BlueprintDocument,
  Component,
  ComponentKind,
  Flow,
} from '@/api/client/types.gen'
import { z } from 'zod'

/** A lowercase slug from a name: `Orders Worker` → `orders-worker`. */
export function slugify(name: string): string {
  const slug = name
    .toLowerCase()
    .normalize('NFKD')
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 40)
    .replace(/-+$/g, '')
  return /^[a-z]/.test(slug) ? slug : `part-${slug}`.replace(/-+$/g, '')
}

/** An id for a new part that no other part uses. */
export function uniqueId(base: string, taken: readonly string[]): string {
  const used = new Set(taken)
  const root = slugify(base) || 'part'
  if (!used.has(root)) return root
  for (let n = 2; ; n += 1) {
    const candidate = `${root}-${n}`
    if (!used.has(candidate)) return candidate
  }
}

/** The fields a part of each kind needs, filled with sensible defaults. */
export function withKind(component: Component, kind: ComponentKind): Component {
  const base = {
    id: component.id,
    name: component.name,
    purpose: component.purpose,
    kind,
  }
  switch (kind) {
    case 'client':
      return { ...base, surface: component.surface ?? 'browser' }
    case 'app':
      return {
        ...base,
        role: component.role ?? 'web',
        runtime: component.runtime ?? 'preset',
        stack: component.stack ?? '',
        ...(component.runtime === 'image' && component.image
          ? { image: component.image }
          : {}),
      }
    case 'data':
      return {
        ...base,
        engine: component.engine ?? 'postgres',
        holds: component.holds ?? [],
      }
    case 'external':
      return { ...base, provider: component.provider ?? '' }
  }
}

export function newComponent(
  kind: ComponentKind,
  name: string,
  taken: readonly string[]
): Component {
  return withKind({ id: uniqueId(name, taken), name, purpose: '', kind }, kind)
}

/**
 * What a data part holds, from the comma-separated text the user typed:
 * `tasks, users` → `['tasks', 'users']`. Blank entries (a trailing comma
 * still being typed) are dropped.
 */
export function parseHolds(text: string): string[] {
  return text
    .split(',')
    .map((entry) => entry.trim())
    .filter(Boolean)
}

/** The text a holds field starts from: `['tasks', 'users']` → `tasks, users`. */
export function formatHolds(
  holds: readonly string[] | null | undefined
): string {
  return (holds ?? []).join(', ')
}

/** Replace one part, keeping flows pointed at it when its id changes. */
export function updateComponent(
  document: BlueprintDocument,
  id: string,
  next: Component
): BlueprintDocument {
  return {
    ...document,
    components: document.components.map((c) => (c.id === id ? next : c)),
    flows: (document.flows ?? []).map((flow) => ({
      ...flow,
      from: flow.from === id ? next.id : flow.from,
      to: flow.to === id ? next.id : flow.to,
    })),
  }
}

/** Remove a part and every flow that touches it. */
export function removeComponent(
  document: BlueprintDocument,
  id: string
): BlueprintDocument {
  return {
    ...document,
    components: document.components.filter((c) => c.id !== id),
    flows: (document.flows ?? []).filter(
      (flow) => flow.from !== id && flow.to !== id
    ),
  }
}

/** The draft as the server wants it: no image unless the app runs one. */
export function cleanDocument(document: BlueprintDocument): BlueprintDocument {
  return {
    summary: document.summary.trim(),
    components: document.components.map((component) => {
      const next: Component = {
        ...component,
        name: component.name.trim(),
        purpose: component.purpose.trim(),
      }
      if (next.kind === 'app' && next.runtime !== 'image') delete next.image
      if (next.kind === 'data') {
        next.holds = (next.holds ?? []).map((h) => h.trim()).filter(Boolean)
      }
      return next
    }),
    flows: (document.flows ?? []).map((flow): Flow => ({
      ...flow,
      label: flow.label.trim(),
    })),
  }
}

/** The problems a 400 from the save endpoint lists, when it lists any. */
export function savedProblems(error: unknown): string[] {
  if (typeof error !== 'object' || error === null) return []
  const problems = (error as { problems?: unknown }).problems
  return Array.isArray(problems)
    ? problems.filter((p): p is string => typeof p === 'string')
    : []
}

const filled = (value: string) => value.trim().length > 0

/**
 * What the editor checks before it sends a design. The server still checks
 * every saved version; these are the mistakes a user can make in the form
 * itself, reported next to the field instead of as a refused save.
 */
export const blueprintEditorSchema = z.object({
  document: z.custom<BlueprintDocument>().superRefine((document, ctx) => {
    if (!filled(document.summary)) {
      ctx.addIssue({
        code: 'custom',
        path: ['summary'],
        message: 'Say what the application does, and for whom',
      })
    }
    if (document.components.length === 0) {
      ctx.addIssue({
        code: 'custom',
        path: ['components'],
        message: 'Add at least one part',
      })
    }
    document.components.forEach((component, index) => {
      if (!filled(component.name)) {
        ctx.addIssue({
          code: 'custom',
          path: ['components', index, 'name'],
          message: `Give the part ${component.id} a name`,
        })
      }
    })
  }),
  changeSummary: z.string().refine(filled, 'Say what you changed'),
})

export type BlueprintEditorValues = z.infer<typeof blueprintEditorSchema>
