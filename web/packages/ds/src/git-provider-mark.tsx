// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { GitBranch } from 'lucide-react'
import GithubIcon from '../../../src/icons/Github'
import GitlabIcon from '../../../src/icons/Gitlab'
import BitbucketIcon from '../../../src/icons/Bitbucket'
import GiteaIcon from '../../../src/icons/Gitea'
import { cn } from './lib/cn'

export interface GitProviderMarkProps {
  provider: string | null | undefined
  className?: string
  /** Omit beside visible provider text; supply only when the mark stands alone. */
  label?: string
}

/** Existing console logos, rendered in currentColor; unknown providers use a branch. */
export function GitProviderMark({
  provider,
  className,
  label,
}: GitProviderMarkProps) {
  const kind = provider?.toLowerCase()
  const Icon =
    kind === 'github' || kind === 'github_app'
      ? GithubIcon
      : kind === 'gitlab'
        ? GitlabIcon
        : kind === 'bitbucket'
          ? BitbucketIcon
          : kind === 'gitea'
            ? GiteaIcon
            : GitBranch
  return (
    <span
      className={cn(
        'inline-flex size-5 shrink-0 items-center justify-center text-current',
        className
      )}
      role={label ? 'img' : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
    >
      <span
        aria-hidden="true"
        className={cn(
          'inline-flex size-full [&>svg]:size-full!',
          Icon !== GitBranch && '[&_path]:fill-current'
        )}
      >
        <Icon className="size-full" />
      </span>
    </span>
  )
}
