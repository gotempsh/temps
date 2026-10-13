// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** How a deployment's source was selected: by branch, by commit, or by tag. */
export type GitReferenceType = 'branch' | 'commit' | 'tag'

/** The Git reference a pipeline trigger is sent with. */
export interface GitTriggerReference {
  branch?: string
  commit?: string
  tag?: string
}

const COMMIT_SHA_PATTERN = /^[0-9a-f]{7,40}$/i

export function isValidCommitSha(commit: string): boolean {
  return COMMIT_SHA_PATTERN.test(commit.trim())
}

/**
 * The reference that rebuilds exactly what an existing deployment ran.
 *
 * Redeploying is "run this deployment again", so it pins the deployment's
 * own commit. Sending only the branch would make the server resolve the
 * branch's *current* head and silently deploy newer code than the row the
 * user clicked. The branch and tag still travel alongside the commit so the
 * new deployment keeps the same label and environment routing.
 *
 * A stored value that is not a commit SHA (the server records a
 * `manual-trigger-<ts>` placeholder when it could not resolve one) cannot be
 * checked out, so only then does a branch or tag deployment fall back to its
 * branch or tag. A deployment made by commit has nothing to fall back to:
 * `null` means it cannot be redeployed as-is, rather than letting the server
 * quietly build the default branch instead.
 */
export function redeployGitReference({
  type,
  branch,
  commit,
  tag,
}: {
  type: GitReferenceType
  branch?: string | null
  commit?: string | null
  tag?: string | null
}): GitTriggerReference | null {
  const sha = commit?.trim()
  const pinned = sha && isValidCommitSha(sha) ? sha.toLowerCase() : undefined
  switch (type) {
    case 'branch':
      return { branch: branch?.trim() || undefined, commit: pinned }
    case 'tag':
      return { tag: tag?.trim() || undefined, commit: pinned }
    case 'commit':
      return pinned ? { commit: pinned } : null
  }
}
