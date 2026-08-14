export type PublicRepositoryLocation = {
  provider: 'github' | 'gitlab'
  owner: string
  name: string
}

export type RepositoryCoordinates = {
  owner: string
  name: string
}

/**
 * Parse an owner/repository reference entered for an authenticated Git
 * connection. URLs and SSH clone references are accepted for convenience,
 * but the selected connection remains the source of credentials.
 */
export function parseRepositoryCoordinates(
  value: string | null | undefined
): RepositoryCoordinates | null {
  let reference = (value || '').trim().replace(/\/$/, '').replace(/\.git$/, '')
  if (!reference) return null

  const sshMatch = reference.match(/^git@[^:]+:(.+)$/)
  if (sshMatch) {
    reference = sshMatch[1]
  } else if (/^https?:\/\//i.test(reference)) {
    try {
      reference = new URL(reference).pathname.replace(/^\//, '')
    } catch {
      return null
    }
  }

  const parts = reference.split('/').filter(Boolean)
  if (parts.length !== 2 || parts.some((part) => /\s/.test(part))) return null

  return { owner: parts[0], name: parts[1] }
}

/** Parse the public GitHub/GitLab URL forms accepted by Temps. */
export function parsePublicRepositoryUrl(
  value: string | null | undefined
): PublicRepositoryLocation | null {
  const match = (value || '')
    .trim()
    .match(
      /(?:https?:\/\/|ssh:\/\/git@|git@)?(github\.com|gitlab\.com)[/:]([^/\s]+)\/([^/\s]+)/i
    )
  if (!match) return null

  const name = match[3].replace(/\.git\/?$/, '').replace(/\/$/, '')
  if (!match[2] || !name) return null
  return {
    provider: match[1].toLowerCase() === 'gitlab.com' ? 'gitlab' : 'github',
    owner: match[2],
    name,
  }
}

export function publicRepositoryProvider(
  value: string | null | undefined
): 'github' | 'gitlab' {
  return parsePublicRepositoryUrl(value)?.provider ?? 'github'
}
