// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProjectResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
import { EmptyState } from '@/components/ui/empty-state'
import { FileQuestion } from 'lucide-react'
import { Link } from 'react-router'

/**
 * A project URL that matches no page: an old bookmark, a mistyped path, or a
 * link to a page that has been removed. Says so and offers the way back,
 * instead of leaving the section navigation beside an empty content area.
 */
export function ProjectPageNotFound({ project }: { project: ProjectResponse }) {
  return (
    <EmptyState
      icon={FileQuestion}
      size="compact"
      title="Page not found"
      description={`This project has no page at this address. It may have moved, or the link may be out of date. Use the navigation, or press ⌘K to search ${project.name}.`}
      action={
        <Button asChild variant="outline">
          <Link to={`/projects/${project.slug}/project`}>
            Go to project overview
          </Link>
        </Button>
      }
    />
  )
}
