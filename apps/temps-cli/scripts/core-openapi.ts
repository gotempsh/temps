// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** External plugin APIs live under /x and must not enter the canonical CLI SDK. */
export function isPluginPath(path: string): boolean {
  return path === '/x' || path.startsWith('/x/')
}

type Document = Record<string, unknown> & {
  paths: Record<string, unknown>
  components?: Record<string, Record<string, unknown>>
}

function references(value: unknown): string[] {
  if (Array.isArray(value)) return value.flatMap(references)
  if (!value || typeof value !== 'object') return []
  const record = value as Record<string, unknown>
  return [
    ...(typeof record.$ref === 'string' &&
    record.$ref.startsWith('#/components/')
      ? [record.$ref]
      : []),
    ...Object.values(record).flatMap(references),
  ]
}

function referencedComponents(value: unknown, document: Document): Set<string> {
  const result = new Set<string>()
  const pending = references(value)
  while (pending.length) {
    const ref = pending.pop()!
    if (result.has(ref)) continue
    result.add(ref)
    const parts = ref
      .slice(2)
      .split('/')
      .map((part) => part.replaceAll('~1', '/').replaceAll('~0', '~'))
    let target: unknown = document
    for (const part of parts) {
      target =
        target && typeof target === 'object'
          ? (target as Record<string, unknown>)[part]
          : undefined
    }
    pending.push(...references(target))
  }
  return result
}

/** Remove plugin operations and only components exclusively owned by those operations. */
export function coreOpenApi(spec: unknown): unknown {
  if (
    !spec ||
    typeof spec !== 'object' ||
    !('paths' in spec) ||
    !spec.paths ||
    typeof spec.paths !== 'object'
  )
    return spec
  const source = spec as Document
  const paths = Object.fromEntries(
    Object.entries(source.paths).filter(([path]) => !isPluginPath(path)),
  )
  const pluginPaths = Object.fromEntries(
    Object.entries(source.paths).filter(([path]) => isPluginPath(path)),
  )
  const candidates = referencedComponents(pluginPaths, source)
  const { components, ...root } = source
  const retained = referencedComponents({ ...root, paths }, source)
  // Keep existing standalone core schemas, including anything they reference.
  // Only components reachable exclusively from the removed plugin paths are pruned.
  for (const [kind, entries] of Object.entries(components ?? {})) {
    for (const [name, definition] of Object.entries(entries)) {
      const ref = `#/components/${kind}/${name.replaceAll('~', '~0').replaceAll('/', '~1')}`
      if (!candidates.has(ref)) {
        for (const dependency of referencedComponents(definition, source))
          retained.add(dependency)
      }
    }
  }
  return {
    ...root,
    paths,
    ...(components
      ? {
          components: Object.fromEntries(
            Object.entries(components).map(([kind, entries]) => [
              kind,
              Object.fromEntries(
                Object.entries(entries).filter(([name]) => {
                  const ref = `#/components/${kind}/${name.replaceAll('~', '~0').replaceAll('/', '~1')}`
                  return !candidates.has(ref) || retained.has(ref)
                }),
              ),
            ]),
          ),
        }
      : {}),
  }
}
