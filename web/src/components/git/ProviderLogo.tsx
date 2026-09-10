// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import GithubIcon from '@/icons/Github'
import GitlabIcon from '@/icons/Gitlab'
import GiteaIcon from '@/icons/Gitea'
import BitbucketIcon from '@/icons/Bitbucket'
import { GitBranch } from 'lucide-react'

/**
 * Real brand logo for a Git provider type, in the provider's brand color
 * (GitLab tanuki orange, Gitea green; GitHub follows the current text color).
 * Unknown/self-hosted "generic" providers fall back to a neutral branch icon.
 *
 * `provider_type` values come from the backend git-provider rows; the GitHub
 * App flavour reports `github_app` and shares the GitHub mark.
 */
export function ProviderLogo({
  providerType,
  className = 'h-4 w-4',
}: {
  providerType: string | null | undefined
  className?: string
}) {
  switch (providerType) {
    case 'github':
    case 'github_app':
      return <GithubIcon className={className} />
    case 'gitlab':
      return <GitlabIcon className={className} />
    case 'gitea':
      return <GiteaIcon className={className} />
    case 'bitbucket':
      return <BitbucketIcon className={className} />
    default:
      return <GitBranch className={className} />
  }
}
