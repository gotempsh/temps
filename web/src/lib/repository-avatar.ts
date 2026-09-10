// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { RepositoryResponse } from '@/api/client'

type RepositoryIdentity = Pick<RepositoryResponse, 'clone_url' | 'owner'>

/**
 * GitHub exposes an account or organization avatar at `/{owner}.png`.
 * Prefer the repository's clone host so GitHub Enterprise connections do not
 * accidentally load artwork from github.com.
 *
 * Other providers do not expose a stable avatar route in the repository data
 * Temps currently stores, so callers should render their neutral fallback.
 */
export function repositoryAvatarUrl(
  repository: RepositoryIdentity,
  providerType?: string | null
): string | null {
  if (providerType?.toLowerCase() !== 'github') return null

  const owner = repository.owner.trim()
  if (!owner || owner.includes('/')) return null

  let origin = 'https://github.com'
  if (repository.clone_url) {
    try {
      const cloneUrl = new URL(repository.clone_url)
      if (cloneUrl.protocol === 'https:') origin = cloneUrl.origin
    } catch {
      // Keep the public GitHub origin for legacy or SSH-shaped clone URLs.
    }
  }

  return `${origin}/${encodeURIComponent(owner)}.png?size=64`
}
