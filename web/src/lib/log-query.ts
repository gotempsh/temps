// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { positiveInteger } from './global-observability'
export const LOG_FILTER_KEYS = [
  'project',
  'source',
  'level',
  'env',
  'node',
  'deployment',
] as const
export const LOG_FILTER_PARAMS = {
  project: 'project_id',
  source: 'source',
  level: 'level',
  env: 'env',
  node: 'node_id',
  deployment: 'deploy_id',
} as const
export type LogProjectChoice = { id: number; name: string; slug: string }
export const quoteLogValue = (value: string) =>
  /\s|["\\]/.test(value) ? JSON.stringify(value) : value

/** Parse supported tokens atomically; unknown key:value text remains a literal message search. */
export function parseLogQuery(
  input: string,
  projects: LogProjectChoice[]
):
  | { patch: Record<string, string | undefined>; error?: undefined }
  | { error: string; patch?: undefined } {
  const patch: Record<string, string | undefined> = {}
  const text: string[] = []
  const parts = input.match(/(?:[^\s"\\]|\\.|"(?:\\.|[^"\\])*")+/g) ?? []
  // An unmatched quote must not silently discard part of a query.
  if ((input.match(/(?<!\\)"/g)?.length ?? 0) % 2)
    return { error: 'Close the quoted value before applying the filter.' }
  for (const part of parts) {
    const match = part.match(
      /^(project|source|level|env|node|deployment):(.*)$/i
    )
    if (!match) {
      text.push(part)
      continue
    }
    const key = match[1].toLowerCase() as keyof typeof LOG_FILTER_PARAMS
    let value = match[2]
    if (value.startsWith('"')) {
      try {
        value = JSON.parse(value) as string
      } catch {
        return { error: `Use a valid quoted value for ${key}.` }
      }
    }
    if (!value) return { error: `Choose a value for ${key}.` }
    if (key === 'level') {
      value = value.toUpperCase()
      if (!['TRACE', 'DEBUG', 'INFO', 'WARN', 'ERROR'].includes(value))
        return { error: 'Choose level:trace, debug, info, warn, or error.' }
    }
    if (key === 'source') {
      value = value.toLowerCase()
      if (!['collected', 'application', 'service'].includes(value))
        return { error: 'Choose source:collected, application, or service.' }
    }
    if (key === 'project') {
      const matches = projects.filter(
        (p) =>
          p.name.toLowerCase() === value.toLowerCase() ||
          p.slug.toLowerCase() === value.toLowerCase()
      )
      const id =
        positiveInteger(value) ??
        (matches.length === 1 ? matches[0].id : undefined)
      if (!id)
        return { error: 'Choose a project suggestion or enter its numeric ID.' }
      value = String(id)
    }
    if (key === 'node' || key === 'deployment') {
      const id = positiveInteger(value)
      if (!id) return { error: `Use a positive numeric ID for ${key}.` }
      value = String(id)
    }
    patch[LOG_FILTER_PARAMS[key]] = value
  }
  if (patch.source === 'service' && patch.project_id)
    return {
      error:
        'Project filters apply to application logs. Remove project or choose source:application.',
    }
  if (patch.source === 'service') patch.project_id = undefined
  else if (patch.project_id && !patch.source) patch.source = 'application'
  patch.q = text.join(' ') || undefined
  return { patch }
}
