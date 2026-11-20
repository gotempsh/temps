// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getProjectOptions } from '@/api/client/@tanstack/react-query.gen'
import { Skeleton } from '@/components/ui/skeleton'
import { useQuery } from '@tanstack/react-query'
import { FolderGit2 } from 'lucide-react'

/**
 * "About <project>" in an assistant chat's header, so it is clear which project
 * the assistant was told this chat is about. A project that can no longer be
 * read (deleted, or access lost) is named generically rather than hidden: the
 * assistant has stopped using it, and the user should see that.
 */
export function FocusProjectLabel({ projectId }: { projectId: number }) {
  const project = useQuery(getProjectOptions({ path: { id: projectId } }))
  if (project.isPending) {
    return <Skeleton className="inline-block h-4 w-24 align-middle" />
  }
  return (
    <span
      className="inline-flex min-w-0 items-center gap-1"
      title="The project this chat was started about"
    >
      <FolderGit2 className="size-3.5 shrink-0" aria-hidden="true" />
      <span className="truncate">
        {project.isError
          ? 'A project you can no longer open'
          : project.data.name}
      </span>
    </span>
  )
}
