export interface ComposeEnvironmentVariableSource {
  key: string
  description?: string | null
}

export interface ComposeServiceEnvironmentSource {
  name: string
  environmentVariables: string[]
}

export interface DiscoveredEnvironmentVariable {
  key: string
  description?: string | null
  sources: string[]
}

/** Merge repository declarations and remove keys Temps already supplies. */
export function discoverComposeEnvironmentVariables({
  configuredKeys,
  envExamplePath,
  envExampleVariables,
  composePath,
  composeServices,
}: {
  configuredKeys: Iterable<string>
  envExamplePath: string
  envExampleVariables: ComposeEnvironmentVariableSource[]
  composePath: string
  composeServices: ComposeServiceEnvironmentSource[]
}): DiscoveredEnvironmentVariable[] {
  const configured = new Set(configuredKeys)
  const discovered = new Map<
    string,
    Omit<DiscoveredEnvironmentVariable, 'sources'> & { sources: Set<string> }
  >()
  const add = (key: string, source: string, description?: string | null) => {
    if (!key || configured.has(key)) return
    const current = discovered.get(key) ?? {
      key,
      description,
      sources: new Set<string>(),
    }
    current.sources.add(source)
    current.description ||= description
    discovered.set(key, current)
  }

  for (const variable of envExampleVariables) {
    add(variable.key, envExamplePath, variable.description)
  }
  for (const service of composeServices) {
    for (const key of service.environmentVariables) {
      add(key, `${composePath} · ${service.name}`)
    }
  }

  return [...discovered.values()]
    .map((variable) => ({
      ...variable,
      sources: [...variable.sources].sort(),
    }))
    .sort((a, b) => a.key.localeCompare(b.key))
}
