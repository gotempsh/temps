// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The reverse of a project's Internal access card (ADR 105): from a sandbox,
 * every project, each allowed in one click to be called by this sandbox. The
 * grant is the project's, so it needs `projects:write` on that project, and
 * it shows on the project's card like any other.
 *
 * Whether a project allows this sandbox comes from that project's sandbox
 * grants, read in full (a project holds at most 100 grants) for exactly the
 * projects on screen.
 */
import {
  useMutation,
  useQueries,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query'
import { Folder } from 'lucide-react'
import { useState } from 'react'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import {
  createInternalAccessSandboxGrant,
  deleteInternalAccessSandboxGrant,
} from '@/api/client'
import { getProjectsOptions } from '@/api/client/@tanstack/react-query.gen'
import type {
  CreateInternalAccessSandboxGrantRequest,
  InternalAccessSandboxGrantListResponse,
  ProjectResponse,
} from '@/api/client/types.gen'
import { ServiceIcon } from '@/components/project/ServiceIcon'
import {
  AllowListPager,
  AllowListRows,
  AllowListSection,
  AllowRow,
  GrantLimitsPopover,
  LookupNotice,
} from '@/components/project/settings/InternalAccessAllowList'
import {
  removalFeedback,
  useAllowGrant,
  useInternalAccessWriteBlock,
} from '@/components/project/settings/internal-access-hooks'
import {
  combinedLookup,
  isSandboxGrantsQuery,
  sandboxGrantsOptions,
  type GrantsLookup,
} from '@/components/project/settings/internal-access-queries'
import {
  GRANTS_PAGE_SIZE,
  callerState,
  limitedSandboxGrant,
  oneClickSandboxGrant,
} from '@/components/project/settings/internal-access'
import { useDebounce } from '@/hooks/useDebounce'

export function SandboxProjectAccess({
  sandboxId,
  sandboxName,
}: {
  sandboxId: string
  sandboxName: string
}) {
  const queryClient = useQueryClient()
  const blockReason = useInternalAccessWriteBlock()
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
  const rows = projects.data?.projects ?? []
  const lookups = useQueries({
    queries: rows.map((project) => sandboxGrantsOptions(project.id)),
  })
  const lookup = combinedLookup(lookups)

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
      note="Allow a project to let this sandbox call its *.temps.local addresses."
    >
      {blockReason ? (
        <p className="text-sm text-muted-foreground">{blockReason}</p>
      ) : null}
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
              ? 'There is no project to allow.'
              : `No project matches “${term}”.`
        }
      >
        {rows.map((project, index) => (
          <ProjectTargetRow
            key={project.id}
            project={project}
            sandboxId={sandboxId}
            sandboxName={sandboxName}
            grants={lookups[index]?.data?.items ?? []}
            lookup={lookup}
            blockReason={blockReason}
            onChanged={() =>
              void queryClient.invalidateQueries({
                predicate: isSandboxGrantsQuery(project.id),
              })
            }
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

function ProjectTargetRow({
  project,
  sandboxId,
  sandboxName,
  grants,
  lookup,
  blockReason,
  onChanged,
}: {
  project: ProjectResponse
  sandboxId: string
  sandboxName: string
  grants: InternalAccessSandboxGrantListResponse['items']
  lookup: GrantsLookup
  blockReason: string | null
  onChanged: () => void
}) {
  const state = callerState(grants, (grant) => grant.sandbox_id === sandboxId)
  const allowMutation = useAllowGrant({
    allow: async (body: CreateInternalAccessSandboxGrantRequest) =>
      createInternalAccessSandboxGrant({
        path: { project_id: project.id },
        body,
        throwOnError: true,
      }),
    onChanged,
    allowed: `Sandbox ${sandboxName} can now call ${project.name}`,
  })
  const [confirming, setConfirming] = useState(false)
  const removeMutation = useMutation({
    mutationFn: async () => {
      for (const grant of state.removable) {
        await deleteInternalAccessSandboxGrant({
          path: { project_id: project.id, grant_id: grant.id },
          throwOnError: true,
        })
      }
    },
    ...removalFeedback(
      onChanged,
      `Sandbox ${sandboxName} can no longer call ${project.name}`
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
        onAllow={() => allowMutation.mutate(oneClickSandboxGrant(sandboxId))}
        onRemove={() => setConfirming(true)}
        limits={
          <GrantLimitsPopover
            name={project.name}
            targetProjectId={project.id}
            targetName={project.name}
            disabled={lookup.state !== 'ready' || allowMutation.isPending}
            pending={allowMutation.isPending}
            onSubmit={(limits) =>
              allowMutation.mutateAsync(limitedSandboxGrant(sandboxId, limits))
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
        consequences={`Calls from this sandbox to ${project.name} get 403 again within seconds.`}
        isPending={removeMutation.isPending}
        onConfirm={() => {
          setConfirming(false)
          removeMutation.mutate()
        }}
      />
    </>
  )
}
