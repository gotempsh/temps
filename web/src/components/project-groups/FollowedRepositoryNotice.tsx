// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Link } from 'react-router'
import { useMutation } from '@tanstack/react-query'
import { toast } from 'sonner'
import { FolderGit2 } from 'lucide-react'
import { setRepositoryInheritanceMutation } from '@/api/client/@tanstack/react-query.gen'
import type { ProjectResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
import { groupHref } from '@/lib/terminology'
import { getErrorMessage } from '@/utils/errorHandling'
import { useServiceGroup } from './ServiceGroupControl'
import { useInvalidateGroups } from './useProjectGroups'

/**
 * On a service's git settings: says when the repository and branch are
 * managed by its project, so an edit here is not a surprise later (the
 * project would put them back the next time its repository changes).
 */
export function FollowedRepositoryNotice({
  project,
  onChanged,
}: {
  project: ProjectResponse
  onChanged?: () => void
}) {
  const membership = useServiceGroup(project.id)
  const invalidate = useInvalidateGroups()
  const stop = useMutation({ ...setRepositoryInheritanceMutation() })
  const group = membership.data?.group
  const self = group?.projects.find((p) => p.id === project.id)
  if (!group?.repository || !self?.inherits_repository) return null
  const repo = group.repository

  const stopFollowing = async () => {
    try {
      await stop.mutateAsync({
        path: { group_id: group.id, project_id: project.id },
        body: { inherit: false },
      })
      await invalidate()
      onChanged?.()
      toast.success(`${project.name} now manages its own repository and branch`)
    } catch (err) {
      toast.error(getErrorMessage(err, 'Could not stop following the project'))
    }
  }

  return (
    <div className="flex flex-col gap-3 rounded-lg border bg-muted/30 px-4 py-3 text-sm sm:flex-row sm:items-center">
      <FolderGit2 className="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
      <p className="flex-1">
        Repository and branch come from project{' '}
        <Link to={groupHref(group.slug)} className="font-medium hover:underline">
          {group.name}
        </Link>{' '}
        (<span className="font-mono">
          {repo.repo_owner}/{repo.repo_name}@{repo.main_branch}
        </span>
        ). Change them there; this service keeps its own directory and build
        settings.
        {self.follows_repository === false &&
          ' Right now it does not match the project; apply the repository again from the project.'}
      </p>
      <div className="flex shrink-0 gap-2">
        <Button asChild size="sm" variant="outline">
          <Link to={groupHref(group.slug)}>Open project</Link>
        </Button>
        <Button size="sm" variant="ghost" disabled={stop.isPending} onClick={() => void stopFollowing()}>
          Stop following
        </Button>
      </div>
    </div>
  )
}
