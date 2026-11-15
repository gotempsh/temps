// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { GitFork } from 'lucide-react'
import BitbucketIcon from '@/icons/Bitbucket'
import GiteaIcon from '@/icons/Gitea'
import GithubIcon from '@/icons/Github'
import GitlabIcon from '@/icons/Gitlab'
import type { GitProviderKind } from '@/lib/project-header-actions'

/**
 * The git host's mark, or a generic fork for one it does not recognise.
 * Decorative: the button or menu item it sits in carries the name.
 */
export function RepositoryProviderIcon({
  provider,
  className,
}: {
  provider: GitProviderKind | null
  className?: string
}) {
  return (
    <span aria-hidden="true" className="contents">
      <ProviderMark provider={provider} className={className} />
    </span>
  )
}

function ProviderMark({
  provider,
  className,
}: {
  provider: GitProviderKind | null
  className?: string
}) {
  if (provider === 'github') return <GithubIcon className={className} />
  if (provider === 'gitlab') return <GitlabIcon className={className} />
  if (provider === 'bitbucket') return <BitbucketIcon className={className} />
  if (provider === 'gitea') return <GiteaIcon className={className} />
  return <GitFork className={className} />
}
