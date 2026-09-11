// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { FolderGit2, LayoutTemplate, Link as LinkIcon } from 'lucide-react'
import { Button } from '@/components/ui/button'

export function TemplateCatalogEmptyState({
  kind,
  onUseGitUrl,
  onBrowseRepositories,
}: {
  kind: 'starter' | 'service'
  onUseGitUrl?: () => void
  onBrowseRepositories?: () => void
}) {
  return (
    <div className="flex flex-col items-center rounded-lg border border-dashed px-6 py-12 text-center">
      <LayoutTemplate className="mb-3 size-8 text-muted-foreground" />
      <p className="font-medium">
        No {kind === 'service' ? 'services' : 'templates'} available
      </p>
      <p className="mt-1 max-w-sm text-sm text-muted-foreground">
        Start from your own source while the catalog is empty.
      </p>
      {onUseGitUrl && onBrowseRepositories && (
        <div className="mt-4 flex flex-wrap justify-center gap-2">
          <Button size="sm" onClick={onUseGitUrl}>
            <LinkIcon className="mr-1.5 size-4" />
            Use a Git URL
          </Button>
          <Button size="sm" variant="outline" onClick={onBrowseRepositories}>
            <FolderGit2 className="mr-1.5 size-4" />
            Browse repositories
          </Button>
        </div>
      )}
    </div>
  )
}
