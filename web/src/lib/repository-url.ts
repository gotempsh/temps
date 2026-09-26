// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Derives a browsable repository URL from its clone URLs. `clone_url` is an
 * HTTPS URL (possibly `.git`-suffixed) for connected providers, but for the
 * "continue with git URL" flow it's the raw string the user typed, which may
 * be SSH shorthand (git@host:owner/repo). Both normalize to
 * https://host/owner/repo; anything else yields null.
 */
export function getRepositoryUrl(repository: {
  clone_url?: string | null
  ssh_url?: string | null
}): string | null {
  const raw = repository.clone_url || repository.ssh_url
  if (!raw) return null
  let url = raw.trim().replace(/\.git$/, '')
  const sshMatch = url.match(/^git@([^:]+):(.+)$/)
  if (sshMatch) {
    url = `https://${sshMatch[1]}/${sshMatch[2]}`
  }
  return url.startsWith('http://') || url.startsWith('https://') ? url : null
}
