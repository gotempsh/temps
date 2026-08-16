export interface CommandDestination {
  id: string
  title: string
  description: string
  url: string
  category: string
  keywords: string[]
}

export interface CommandExampleResources {
  projectSlugs: string[]
  services: Array<{ name: string; serviceType: string }>
}

/**
 * Keep one canonical destination for each URL so duplicate labels from the
 * sidebar, plugins, and cross-project search do not create repeated results.
 */
export function dedupeCommandDestinations(
  destinations: CommandDestination[]
): CommandDestination[] {
  const byUrl = new Map<string, CommandDestination>()
  for (const destination of destinations) {
    if (!byUrl.has(destination.url)) byUrl.set(destination.url, destination)
  }
  return [...byUrl.values()]
}

/**
 * Explicit resource identifiers are constraints, not hints. A tiny model may
 * understand "production environment" yet choose another page in the named
 * project when the tool catalog is large. Resolve this safety-critical case
 * before inference so the written slug always wins over ambient context.
 */
export function resolveExplicitProjectEnvironment(
  query: string,
  projectSlugs: string[],
  destinations: CommandDestination[],
  currentProjectSlug?: string | null
): CommandDestination | undefined {
  const normalized = query.toLowerCase()
  if (
    !normalized.includes('environment') ||
    !/\bprod(?:uction)?\b/.test(normalized)
  ) {
    return undefined
  }

  const sortedSlugs = [...projectSlugs].sort((a, b) => b.length - a.length)
  const namedSlug = sortedSlugs.find((candidate) =>
    normalized.includes(candidate.toLowerCase())
  )
  const queryTerms = normalized
    .replace(/[^a-z0-9-]+/g, ' ')
    .trim()
    .split(/\s+/)
    .filter(
      (term) =>
        term.length > 2 &&
        term !== 'environment' &&
        term !== 'production' &&
        term !== 'prod'
    )
  const partialSlugMatches = namedSlug
    ? []
    : sortedSlugs.filter((candidate) =>
        queryTerms.some((term) =>
          candidate.toLowerCase().split('-').includes(term)
        )
      )
  // A short project reference such as "api" is safe only when it identifies
  // one project. Ambiguous partials remain search hints instead of constraints.
  const uniquePartialSlug =
    partialSlugMatches.length === 1 ? partialSlugMatches[0] : undefined
  const slug =
    namedSlug ??
    uniquePartialSlug ??
    (normalized.includes('this project') ||
    normalized.includes('current project')
      ? currentProjectSlug
      : undefined)
  if (!slug) return undefined

  return destinations.find(
    (destination) => destination.id === `project:${slug}:environment:production`
  )
}

export function resolveExplicitNamedProjectDestination(
  query: string,
  projectSlugs: string[],
  rankedDestinations: CommandDestination[]
): CommandDestination | undefined {
  const normalized = query.toLowerCase()
  const slug = [...projectSlugs]
    .sort((a, b) => b.length - a.length)
    .find((candidate) => normalized.includes(candidate.toLowerCase()))
  if (!slug) return undefined

  return rankedDestinations.find((destination) =>
    destination.id.startsWith(`project:${slug}:`)
  )
}

export function toCommandExtendedQuery(query: string): string {
  const stopWords = new Set([
    'a',
    'an',
    'can',
    'for',
    'me',
    'my',
    'open',
    'please',
    'show',
    'take',
    'the',
    'this',
    'to',
    'want',
    'where',
    'with',
  ])
  return query
    .toLowerCase()
    .replace(/[^a-z0-9-]+/g, ' ')
    .trim()
    .split(/\s+/)
    .filter((term) => term.length > 1 && !stopWords.has(term))
    .map((term) => `'${term}`)
    .join(' ')
}

export function buildCommandSampleQueries({
  projectSlugs,
  services,
}: CommandExampleResources): string[] {
  const [primaryProject, secondaryProject] = projectSlugs
  const postgres = services.find(
    (service) => service.serviceType.toLowerCase() === 'postgres'
  )

  return [
    'Show me the latest S3 backups',
    postgres
      ? `Open the PostgreSQL service ${postgres.name}`
      : 'Show me all database services',
    primaryProject
      ? `Open the production environment for ${primaryProject}`
      : 'Show me all projects',
    secondaryProject
      ? `Show visitors for ${secondaryProject}`
      : primaryProject
        ? `Show errors for ${primaryProject}`
        : 'Open platform monitoring',
  ]
}
