// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

const HTTP_METHODS = new Set([
  'GET',
  'HEAD',
  'POST',
  'PUT',
  'PATCH',
  'DELETE',
  'OPTIONS',
  'CONNECT',
  'TRACE',
])
const MAX_TARGET = 56

/**
 * What a span acted on, for a span whose name alone does not say.
 *
 * OpenTelemetry names an HTTP client span after its method ("POST") and many
 * database clients after the operation ("GET", "SELECT"), so a waterfall of
 * those reads as a list of verbs. The target comes from the span's own
 * attributes: the URL for HTTP, the statement for a database. A span whose
 * name already carries more than the verb gets nothing.
 */
export function spanTarget(
  name: string,
  attributes: Record<string, string> | null | undefined
): string | null {
  const attrs = attributes ?? {}
  const verb = name.trim()
  if (!verb || /\s/.test(verb)) return null

  const operation = attrs['db.operation.name'] ?? attrs['db.operation']
  const statement = attrs['db.query.text'] ?? attrs['db.statement']
  const system = attrs['db.system.name'] ?? attrs['db.system']
  if (statement && operation?.toUpperCase() === verb.toUpperCase()) {
    const rest = statement.startsWith(`${operation} `)
      ? statement.slice(operation.length + 1)
      : statement
    return clip(system ? `${system} · ${rest}` : rest)
  }

  if (!HTTP_METHODS.has(verb.toUpperCase())) return null
  const full = attrs['url.full'] ?? attrs['http.url']
  if (full) {
    try {
      const url = new URL(full)
      return clip(`${url.host}${url.pathname === '/' ? '' : url.pathname}`)
    } catch {
      return clip(full)
    }
  }
  const host = attrs['server.address'] ?? ''
  const path = attrs['url.path'] ?? attrs['http.route'] ?? attrs['http.target']
  const target = `${host}${path ?? ''}`
  return target ? clip(target) : null
}

function clip(text: string) {
  return text.length > MAX_TARGET ? `${text.slice(0, MAX_TARGET - 1)}…` : text
}
