export type PublicRepositoryLocation = {
  provider: 'github' | 'gitlab'
  owner: string
  name: string
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
