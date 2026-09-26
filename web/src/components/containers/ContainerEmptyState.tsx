// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Boxes } from 'lucide-react'
import { Link } from 'react-router'
import { Button } from '@/components/ui/button'

export function ContainerEmptyState({ projectSlug }: { projectSlug: string }) {
  return (
    <div className="flex flex-col items-center rounded-lg border border-dashed px-6 py-10 text-center">
      <Boxes className="mb-3 size-8 text-muted-foreground" />
      <p className="font-medium">No containers yet</p>
      <p className="mt-1 max-w-sm text-sm text-muted-foreground">
        Containers appear after a deployment starts. Check deployment history to
        see whether a build is running or needs attention.
      </p>
      <Button asChild size="sm" className="mt-4">
        <Link to={`/projects/${projectSlug}/deployments`}>
          View deployments
        </Link>
      </Button>
    </div>
  )
}
