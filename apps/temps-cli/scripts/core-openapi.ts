// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

type Document = {
  paths: Record<string, unknown>
  components?: { schemas?: Record<string, unknown>; [key: string]: unknown }
  [key: string]: unknown
}

function references(value: unknown): Set<string> {
  const result = new Set<string>()
  const visit = (node: unknown) => {
    if (!node || typeof node !== 'object') return
    if (Array.isArray(node)) {
      node.forEach(visit)
      return
    }
    for (const [key, child] of Object.entries(node)) {
      if (
        key === '$ref' &&
        typeof child === 'string' &&
        child.startsWith('#/components/schemas/')
      ) {
        result.add(child.slice('#/components/schemas/'.length))
      } else visit(child)
    }
  }
  visit(value)
  return result
}

/** Plugin APIs use handwritten clients. Preserve historical entries without importing new plugin APIs. */
export function coreOpenApi(fetched: Document, committed: Document): Document {
  const corePaths = Object.fromEntries(
    Object.entries(fetched.paths).filter(([path]) => !path.startsWith('/x/')),
  )
  const historicalPluginPaths = Object.fromEntries(
    Object.entries(committed.paths).filter(([path]) => path.startsWith('/x/')),
  )
  const fetchedSchemas = fetched.components?.schemas ?? {}
  const oldSchemas = committed.components?.schemas ?? {}
  const closure = (roots: unknown, schemas: Record<string, unknown>) => {
    const names = references(roots)
    for (const name of names)
      for (const child of references(schemas[name])) names.add(child)
    return names
  }
  const coreNames = closure(corePaths, fetchedSchemas)
  const historicalNames = closure(historicalPluginPaths, oldSchemas)
  const schemas = { ...oldSchemas }
  for (const name of coreNames)
    if (name in fetchedSchemas) schemas[name] = fetchedSchemas[name]
  // Historical plugin-only schema shapes stay frozen, just like their paths.
  for (const name of historicalNames)
    if (!coreNames.has(name) && name in oldSchemas)
      schemas[name] = oldSchemas[name]
  return {
    ...fetched,
    paths: { ...corePaths, ...historicalPluginPaths },
    components: { ...fetched.components, schemas },
  }
}
