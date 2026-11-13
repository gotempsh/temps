// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Project callers on the Internal access card (ADR 101): every other project,
 * each allowed in one click to call this project (or the environment this
 * card is for) from all its environments.
 */
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Folder } from 'lucide-react'
import { useState } from 'react'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import {
  createInternalAccessGrant,
  deleteInternalAccessGrant,
} from '@/api/client'
import { getProjectsOptions } from '@/api/client/@tanstack/react-query.gen'
import type {
  CreateInternalAccessGrantRequest,
  InternalAccessGrantResponse,
  ProjectResponse,
} from '@/api/client/types.gen'
import { ServiceIcon } from '@/components/project/ServiceIcon'
import { useDebounce } from '@/hooks/useDebounce'
import {
  AllowListPager,
  AllowListRows,
  AllowListSection,
  AllowRow,
  GrantLimitsPopover,
  LookupNotice,
} from './InternalAccessAllowList'
import {
  GRANTS_PAGE_SIZE,
  callerState,
  limitedProjectGrant,
  oneClickProjectGrant,
} from './internal-access'
import { removalFeedback, useAllowGrant } from './internal-access-hooks'
import {
  isProjectGrantsQuery,
  lookupOf,
  projectGrantsOptions,
  type GrantsLookup,
} from './internal-access-queries'

export function InternalAccessProjectCallers({
  projectId,
  targetName,
  environmentId,
  blockReason,
}: {
  projectId: number
  /** "Orders API", or "Orders API staging" on an environment's card. */
  targetName: string
  environmentId?: number
  blockReason: string | null
}) {
  const queryClient = useQueryClient()
  const [search, setSearch] = useState('')
  const [page, setPage] = useState(1)
  const term = useDebounce(search.trim(), 250)
  const projects = useQuery(
    getProjectsOptions({
      query: {
        page,
        per_page: GRANTS_PAGE_SIZE,
        ...(term === '' ? {} : { search: term }),
      },
    })
  )
  const grants = useQuery(projectGrantsOptions(projectId, environmentId))
  const lookup = lookupOf(grants)
  const rows = (projects.data?.projects ?? []).filter(
    (project) => project.id !== projectId
  )
  const onChanged = () =>
    void queryClient.invalidateQueries({
      predicate: isProjectGrantsQuery(projectId),
    })

  return (
    <AllowListSection
      title="Projects"
      icon={Folder}
      search={search}
      onSearchChange={(value) => {
        setSearch(value)
        setPage(1)
      }}
      searchLabel="Search projects"
    >
      <LookupNotice lookup={lookup} plural="projects" />
      <AllowListRows
        isPending={projects.isPending}
        isError={projects.isError}
        error={projects.error}
        retry={() => void projects.refetch()}
        retrying={projects.isFetching}
        plural="Projects"
        empty={
          rows.length > 0
            ? null
            : term === ''
              ? 'There is no other project to allow.'
              : `No project matches “${term}”.`
        }
      >
        {rows.map((project) => (
          <ProjectCallerRow
            key={project.id}
            project={project}
            projectId={projectId}
            targetName={targetName}
            environmentId={environmentId}
            grants={grants.data?.items ?? []}
            lookup={lookup}
            blockReason={blockReason}
            onChanged={onChanged}
          />
        ))}
      </AllowListRows>
      <AllowListPager
        page={page}
        total={projects.data?.total ?? 0}
        onPageChange={setPage}
        ariaLabel="Project pages"
      />
    </AllowListSection>
  )
}

function ProjectCallerRow({
  project,
  projectId,
  targetName,
  environmentId,
  grants,
  lookup,
  blockReason,
  onChanged,
}: {
  project: ProjectResponse
  projectId: number
  targetName: string
  environmentId?: number
  grants: InternalAccessGrantResponse[]
  lookup: GrantsLookup
  blockReason: string | null
  onChanged: () => void
}) {
  const state = callerState(
    grants,
    (grant) => grant.caller_project_id === project.id,
    environmentId
  )
  const allowMutation = useAllowGrant({
    allow: async (body: CreateInternalAccessGrantRequest) =>
      createInternalAccessGrant({
        path: { project_id: projectId },
        body,
        throwOnError: true,
      }),
    onChanged,
    allowed: `${project.name} can now call ${targetName}`,
  })
  const [confirming, setConfirming] = useState(false)
  const removeMutation = useMutation({
    mutationFn: async () => {
      for (const grant of state.removable) {
        await deleteInternalAccessGrant({
          path: { project_id: projectId, grant_id: grant.id },
          throwOnError: true,
        })
      }
    },
    ...removalFeedback(
      onChanged,
      `${project.name} can no longer call ${targetName}`
    ),
  })
  return (
    <>
      <AllowRow
        icon={<ServiceIcon service={project} size="xs" />}
        name={project.name}
        state={state}
        lookup={lookup}
        blockReason={blockReason}
        allowing={allowMutation.isPending}
        removing={removeMutation.isPending}
        error={
          allowMutation.isError
            ? allowMutation.error
            : removeMutation.isError
              ? removeMutation.error
              : null
        }
        onAllow={() =>
          allowMutation.mutate(oneClickProjectGrant(project.id, environmentId))
        }
        onRemove={() => setConfirming(true)}
        limits={
          <GrantLimitsPopover
            name={project.name}
            callerProjectId={project.id}
            targetProjectId={projectId}
            targetName={targetName}
            fixedTargetEnvironmentId={environmentId}
            disabled={lookup.state !== 'ready' || allowMutation.isPending}
            pending={allowMutation.isPending}
            onSubmit={(limits) =>
              allowMutation.mutateAsync(limitedProjectGrant(project.id, limits))
            }
          />
        }
      />
      <ConfirmDeleteDialog
        open={confirming}
        onOpenChange={setConfirming}
        resourceType="internal access"
        resourceName={project.name}
        verb="Remove"
        irreversible={false}
        consequences={`Calls from ${project.name} to ${targetName} get 403 again within seconds.`}
        isPending={removeMutation.isPending}
        onConfirm={() => {
          setConfirming(false)
          removeMutation.mutate()
        }}
      />
    </>
  )
}
