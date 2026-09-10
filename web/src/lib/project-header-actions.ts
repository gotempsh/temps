// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export type GitProviderKind = 'github' | 'gitlab' | 'bitbucket' | 'gitea'

export function repositoryWebUrl(url: string): string | null {
  const trimmed = url.trim()
  if (!trimmed) return null

  try {
    const scpStyle = trimmed.match(/^[^@\s]+@([^:/\s]+):(.+)$/)
    const sourceUrl = scpStyle
      ? new URL(`https://${scpStyle[1]}/${scpStyle[2]}`)
      : new URL(trimmed)
    const parsed =
      sourceUrl.protocol === 'ssh:'
        ? new URL(
            `https://${sourceUrl.hostname}${sourceUrl.pathname}${sourceUrl.search}${sourceUrl.hash}`
          )
        : sourceUrl
    if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') return null

    // Clone URLs are link inputs, not credential transports. Legacy/provider
    // records may still contain userinfo or token-like query parameters; never
    // expose those in the DOM, browser history, referrers, or destination logs.
    parsed.username = ''
    parsed.password = ''
    parsed.search = ''
    parsed.hash = ''
    parsed.pathname = parsed.pathname.replace(/\.git\/?$/, '')
    return parsed.toString().replace(/\/$/, '')
  } catch {
    return null
  }
}

export function gitProviderFromUrl(url: string): GitProviderKind | null {
  const repositoryUrl = repositoryWebUrl(url)
  if (!repositoryUrl) return null

  const hostname = new URL(repositoryUrl).hostname.toLowerCase()
  if (hostname === 'github.com') return 'github'
  if (hostname === 'gitlab.com') return 'gitlab'
  if (hostname === 'bitbucket.org') return 'bitbucket'
  if (hostname === 'gitea.com') return 'gitea'

  return null
}

export function gitProviderKind(
  providerType: string | null | undefined,
  repositoryUrl: string
): GitProviderKind | null {
  const normalizedType = providerType?.toLowerCase()
  if (normalizedType?.includes('github')) return 'github'
  if (normalizedType?.includes('gitlab')) return 'gitlab'
  if (normalizedType?.includes('bitbucket')) return 'bitbucket'
  if (normalizedType?.includes('gitea')) return 'gitea'

  return gitProviderFromUrl(repositoryUrl)
}

export function branchCommitSha(
  branches: ReadonlyArray<{ name: string; commit_sha?: string }>,
  branchName: string
): string | null {
  return (
    branches.find((branch) => branch.name === branchName)?.commit_sha ?? null
  )
}
