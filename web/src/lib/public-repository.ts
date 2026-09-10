// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export type PublicRepositoryLocation = {
  provider: 'github' | 'gitlab'
  owner: string
  name: string
  /** GitLab origin for self-hosted instances; omitted for gitlab.com. */
  instanceUrl?: string
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
  let reference = (value || '')
    .trim()
    .replace(/\/$/, '')
    .replace(/\.git$/, '')
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
  const raw = (value || '')
    .trim()
    .replace(/\/$/, '')
    .replace(/\.git$/, '')
  if (!raw) return null
  if (raw.includes('://') && !/^(?:https?|ssh):\/\//i.test(raw)) return null

  let hostname: string
  let pathname: string
  let origin: string
  try {
    if (/^https?:\/\//i.test(raw) || /^ssh:\/\//i.test(raw)) {
      const parsed = new URL(raw)
      hostname = parsed.hostname.toLowerCase()
      pathname = parsed.pathname
      origin = `https://${parsed.host}`
    } else {
      const ssh = raw.match(/^git@([^:]+):(.+)$/i)
      if (ssh) {
        hostname = ssh[1].toLowerCase()
        pathname = `/${ssh[2]}`
        origin = `https://${ssh[1]}`
      } else {
        const schemeless = raw.match(/^([^/]+)\/(.+)$/)
        if (!schemeless) return null
        hostname = schemeless[1].toLowerCase()
        pathname = `/${schemeless[2]}`
        origin = `https://${schemeless[1]}`
      }
    }
  } catch {
    return null
  }

  // GitHub public clone URLs use github.com. Any other HTTPS/SSH host is
  // treated as a self-hosted GitLab origin; GitLab instances frequently use
  // neutral corporate hostnames that do not contain the word "gitlab".
  const provider = hostname === 'github.com' ? 'github' : 'gitlab'

  const parts = pathname.split('/').filter(Boolean)
  if (parts.length < 2 || (provider === 'github' && parts.length !== 2)) {
    return null
  }
  const name = parts.pop()
  if (!name) return null
  const owner = parts.join('/')

  return {
    provider,
    owner,
    name,
    ...(provider === 'gitlab' && hostname !== 'gitlab.com'
      ? { instanceUrl: origin }
      : {}),
  }
}

export function publicRepositoryProvider(
  value: string | null | undefined
): 'github' | 'gitlab' {
  return parsePublicRepositoryUrl(value)?.provider ?? 'github'
}
