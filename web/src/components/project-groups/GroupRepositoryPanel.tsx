// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { Link } from 'react-router'
import { useMutation } from '@tanstack/react-query'
import { toast } from 'sonner'
import {
  AlertTriangle,
  Check,
  FolderGit2,
  GitBranch,
  Globe,
  MoreHorizontal,
  Plus,
} from 'lucide-react'
import {
  setGroupRepositoryMutation,
  setRepositoryInheritanceMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  ProjectGroupMemberResponse,
  ProjectGroupResponse,
} from '@/api/client'
import { ServiceIcon } from '@/components/project/ServiceIcon'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { getErrorMessage } from '@/utils/errorHandling'
import { DisconnectRepositoryDialog } from './DisconnectRepositoryDialog'
import { normalizeDirectory } from './repository-import'
import { useInvalidateGroups } from './useProjectGroups'

interface GroupRepositoryPanelProps {
  group: ProjectGroupResponse
  onConnect: () => void
  onImport: () => void
}

/**
 * The project's repository and how each service relates to it: the one
 * place to see which directory each app deploys from, and to bring a
 * service back in line when it drifted.
 */
export function GroupRepositoryPanel({
  group,
  onConnect,
  onImport,
}: GroupRepositoryPanelProps) {
  const repo = group.repository
  const invalidate = useInvalidateGroups()
  const [confirmDisconnect, setConfirmDisconnect] = useState(false)
  const reapply = useMutation({ ...setGroupRepositoryMutation() })
  const follow = useMutation({ ...setRepositoryInheritanceMutation() })

  if (!repo) {
    return (
      <section
        aria-labelledby="group-repository"
        className="flex flex-col gap-4 rounded-lg border border-dashed px-5 py-5 sm:flex-row sm:items-center"
      >
        <div className="flex size-10 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground">
          <FolderGit2 className="size-5" />
        </div>
        <div className="flex-1 space-y-1">
          <h2 id="group-repository" className="font-semibold">
            Deploy several services from one repository
          </h2>
          <p className="text-sm text-muted-foreground">
            Using a monorepo? Connect it once, then add each app in it (for
            example <code>apps/web</code> and <code>apps/api</code>) as a
            service. Services follow the project&apos;s branch, so changing it
            here changes it for all of them.
          </p>
        </div>
        <Button onClick={onConnect} className="shrink-0">
          Connect repository
        </Button>
      </section>
    )
  }

  const repoLabel = `${repo.repo_owner}/${repo.repo_name}`
  const sameRepo = (p: ProjectGroupMemberResponse) =>
    p.repo_owner === repo.repo_owner && p.repo_name === repo.repo_name
  const drifted = group.projects.filter(
    (p) => p.inherits_repository && p.follows_repository === false
  )
  const followers = group.projects.filter((p) => p.inherits_repository)

  const isPublic = Boolean(repo.public_provider)
  const connectionMissing = !repo.git_provider_connection_id && !isPublic

  const retry = async () => {
    if (connectionMissing) {
      onConnect()
      return
    }
    try {
      const result = await reapply.mutateAsync({
        path: { group_id: group.id },
        body: {
          ...(isPublic
            ? { public_provider: repo.public_provider }
            : { git_provider_connection_id: repo.git_provider_connection_id }),
          repo_owner: repo.repo_owner,
          repo_name: repo.repo_name,
          main_branch: repo.main_branch,
        },
      })
      await invalidate()
      const failed = result.services.filter((s) => s.error)
      if (failed.length > 0) {
        toast.error(`${failed[0].name}: ${failed[0].error}`)
      } else {
        toast.success(
          `Every service now follows ${repoLabel}@${repo.main_branch}`
        )
      }
    } catch (err) {
      toast.error(getErrorMessage(err, 'Could not apply the repository again'))
    }
  }

  const setFollowing = async (
    service: ProjectGroupMemberResponse,
    inherit: boolean
  ) => {
    try {
      await follow.mutateAsync({
        path: { group_id: group.id, project_id: service.id },
        body: { inherit },
      })
      await invalidate()
      toast.success(
        inherit
          ? `${service.name} now follows ${repoLabel}@${repo.main_branch}`
          : `${service.name} keeps its settings and no longer follows the project's repository`
      )
    } catch (err) {
      toast.error(getErrorMessage(err, `Could not update ${service.name}`))
    }
  }

  return (
    <section aria-labelledby="group-repository" className="rounded-lg border">
      <div className="flex flex-wrap items-center gap-3 border-b px-4 py-3">
        <FolderGit2
          className="size-5 shrink-0 text-muted-foreground"
          aria-hidden="true"
        />
        <div className="min-w-0 flex-1">
          <h2
            id="group-repository"
            className="flex flex-wrap items-center gap-2"
          >
            <span className="truncate font-mono text-sm font-semibold">
              {repoLabel}
            </span>
            <Badge variant="secondary" className="gap-1 font-mono font-normal">
              <GitBranch className="size-3" aria-hidden="true" />
              {repo.main_branch}
            </Badge>
            {isPublic && (
              <Badge variant="outline" className="gap-1 font-normal">
                <Globe className="size-3" aria-hidden="true" />
                Public
              </Badge>
            )}
          </h2>
          <p className="text-xs text-muted-foreground">
            {followers.length === 0
              ? 'No service follows this repository yet.'
              : isPublic
                ? `${followers.length} of ${group.projects.length} services deploy from it. Deploy them from each service; pushes to a public repository do not trigger deploys.`
                : `${followers.length} of ${group.projects.length} services deploy from it. Pushes to ${repo.main_branch} deploy them.`}
          </p>
        </div>
        <Button size="sm" onClick={onImport}>
          <Plus className="size-4" />
          Add services from repository
        </Button>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              variant="ghost"
              size="icon"
              className="size-8"
              aria-label="Repository actions"
            >
              <MoreHorizontal className="size-4" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuItem onSelect={onConnect}>
              Change repository or branch…
            </DropdownMenuItem>
            <DropdownMenuItem
              className="text-destructive focus:text-destructive"
              onSelect={() => setConfirmDisconnect(true)}
            >
              Disconnect repository…
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>

      {connectionMissing && (
        <div
          role="alert"
          className="flex flex-wrap items-center gap-3 border-b bg-destructive/5 px-4 py-2.5 text-sm"
        >
          <AlertTriangle className="size-4 shrink-0 text-destructive" />
          <p className="flex-1">
            The git connection for {repoLabel} was removed, so its services
            cannot be updated from here. Connect the repository again.
          </p>
          <Button size="sm" variant="outline" onClick={onConnect}>
            Connect again
          </Button>
        </div>
      )}
      {drifted.length > 0 && (
        <div
          role="alert"
          className="flex flex-wrap items-center gap-3 border-b bg-warning/10 px-4 py-2.5 text-sm"
        >
          <AlertTriangle className="size-4 shrink-0 text-warning" />
          <p className="flex-1">
            {drifted.map((p) => p.name).join(', ')}{' '}
            {drifted.length === 1 ? 'is' : 'are'} not on {repoLabel}@
            {repo.main_branch}. A change could not be applied, or their git
            settings were edited directly.
          </p>
          <Button
            size="sm"
            variant="outline"
            onClick={() => void retry()}
            disabled={reapply.isPending}
          >
            {reapply.isPending ? 'Applying…' : 'Apply again'}
          </Button>
        </div>
      )}

      {group.projects.length === 0 ? (
        <p className="px-4 py-4 text-sm text-muted-foreground">
          No services yet.{' '}
          <button
            type="button"
            className="font-medium text-foreground underline"
            onClick={onImport}
          >
            Add the apps in {repoLabel}
          </button>{' '}
          as services.
        </p>
      ) : (
        <ul className="divide-y">
          {group.projects.map((p) => (
            <li
              key={p.id}
              className="flex flex-wrap items-center gap-3 px-4 py-2"
            >
              <ServiceIcon service={p} size="sm" />
              <Link
                to={`/projects/${p.slug}`}
                className="min-w-24 text-sm font-medium hover:underline"
              >
                {p.name}
              </Link>
              <span className="min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground">
                {sameRepo(p)
                  ? normalizeDirectory(p.directory) === '.'
                    ? '(repository root)'
                    : normalizeDirectory(p.directory)
                  : p.repo_owner
                    ? `${p.repo_owner}/${p.repo_name}`
                    : 'Not deployed from Git'}
              </span>
              <MemberStatus
                member={p}
                branch={repo.main_branch}
                sameRepo={sameRepo(p)}
                busy={follow.isPending}
                onFollow={() => void setFollowing(p, true)}
                onUnfollow={() => void setFollowing(p, false)}
              />
            </li>
          ))}
        </ul>
      )}

      <DisconnectRepositoryDialog
        group={group}
        open={confirmDisconnect}
        onOpenChange={setConfirmDisconnect}
      />
    </section>
  )
}

function MemberStatus({
  member,
  branch,
  sameRepo,
  busy,
  onFollow,
  onUnfollow,
}: {
  member: ProjectGroupMemberResponse
  branch: string
  sameRepo: boolean
  busy: boolean
  onFollow: () => void
  onUnfollow: () => void
}) {
  if (member.inherits_repository) {
    return (
      <span className="flex items-center gap-2 text-xs">
        {member.follows_repository === false ? (
          <span className="flex items-center gap-1 text-warning">
            <AlertTriangle className="size-3.5" /> Not on {branch}
          </span>
        ) : (
          <span className="flex items-center gap-1 text-muted-foreground">
            <Check className="size-3.5" /> Follows {branch}
          </span>
        )}
        <Button
          variant="ghost"
          size="sm"
          className="h-7 px-2 text-xs"
          disabled={busy}
          onClick={onUnfollow}
        >
          Stop following
        </Button>
      </span>
    )
  }
  // Only a service already built from this repository is offered to follow
  // it; pointing a service with its own repository here would swap the code
  // it deploys, which is not a one-click decision.
  if (sameRepo) {
    return (
      <span className="flex items-center gap-2 text-xs text-muted-foreground">
        On {member.main_branch}, set on the service
        <Button
          variant="outline"
          size="sm"
          className="h-7 px-2 text-xs"
          disabled={busy}
          onClick={onFollow}
        >
          Follow project
        </Button>
      </span>
    )
  }
  if (member.source_type === 'git') {
    return (
      <span className="text-xs text-muted-foreground">Its own repository</span>
    )
  }
  return (
    <span className="text-xs text-muted-foreground">Not deployed from Git</span>
  )
}
