// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Sandbox callers on the Internal access card (ADR 105): the user's
 * sandboxes, running ones first, each allowed in one click to call this
 * project's `*.temps.local` addresses. A sandbox calls them with plain
 * `http://` through its egress proxy, in every egress mode; without a grant
 * it gets a 403 naming this card.
 *
 * `listSandboxes` has no search or status order, so the list asks for the
 * user's newest sandboxes in one request (its limit) and searches, orders
 * and pages them here; past that limit it says so.
 */
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Box } from 'lucide-react'
import { useState } from 'react'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { problemDetail } from '@/lib/api-problem'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import {
  createInternalAccessSandboxGrant,
  deleteInternalAccessSandboxGrant,
} from '@/api/client'
import type {
  CreateInternalAccessSandboxGrantRequest,
  InternalAccessSandboxGrantResponse,
  SandboxInner,
} from '@/api/client/types.gen'
import {
  AllowListPager,
  AllowListRows,
  AllowListSection,
  AllowRow,
  GrantLimitsPopover,
  LookupNotice,
} from './InternalAccessAllowList'
import {
  SANDBOX_LOOKUP_LIMIT,
  callerState,
  clampGrantsPage,
  GRANTS_PAGE_SIZE,
  limitedSandboxGrant,
  namedSandbox,
  oneClickSandboxGrant,
  pageOf,
  sandboxesForAllowList,
} from './internal-access'
import { removalFeedback, useAllowGrant } from './internal-access-hooks'
import {
  isSandboxGrantsQuery,
  lookupOf,
  sandboxGrantsOptions,
  userSandboxesOptions,
  type GrantsLookup,
} from './internal-access-queries'

export function InternalAccessSandboxGrants({
  projectId,
  targetName,
  environmentId,
  blockReason,
}: {
  projectId: number
  targetName: string
  environmentId?: number
  blockReason: string | null
}) {
  const queryClient = useQueryClient()
  const [search, setSearch] = useState('')
  const [page, setPage] = useState(1)
  const sandboxes = useQuery(userSandboxesOptions())
  const grants = useQuery(sandboxGrantsOptions(projectId, environmentId))
  const lookup = lookupOf(grants)
  // Existing callers come from the project's grants, independently of the
  // bounded, owner-scoped choices for new access. Never fetch another owner's
  // sandbox details just to render or revoke a grant.
  const callers = new Map<string, SandboxCaller>(
    (sandboxes.data?.sandboxes ?? []).map((sandbox) => [sandbox.id, sandbox])
  )
  for (const grant of grants.data?.items ?? []) {
    if (!callers.has(grant.sandbox_id)) {
      callers.set(grant.sandbox_id, {
        id: grant.sandbox_id,
        name: grant.sandbox_name,
        status: grant.sandbox_status,
      })
    }
  }
  const listed = sandboxesForAllowList([...callers.values()], search)
  const shownPage = clampGrantsPage(page, listed.length, GRANTS_PAGE_SIZE)
  const truncated =
    (sandboxes.data?.pagination.count ?? 0) > SANDBOX_LOOKUP_LIMIT
  const onChanged = () =>
    void queryClient.invalidateQueries({
      predicate: isSandboxGrantsQuery(projectId),
    })

  return (
    <AllowListSection
      title="Sandboxes"
      icon={Box}
      search={search}
      onSearchChange={(value) => {
        setSearch(value)
        setPage(1)
      }}
      searchLabel="Search sandbox callers"
      note={
        truncated
          ? `Existing callers and your ${SANDBOX_LOOKUP_LIMIT} newest sandboxes, running ones first; search covers these.`
          : 'Existing callers and your sandboxes, running ones first.'
      }
    >
      <LookupNotice lookup={lookup} plural="sandboxes" />
      {sandboxes.isError && listed.length > 0 ? (
        <ErrorAlert
          title="Could not load your sandboxes"
          description={problemDetail(
            sandboxes.error,
            'New sandbox choices could not be loaded.'
          )}
          retry={() => void sandboxes.refetch()}
          retrying={sandboxes.isFetching}
        />
      ) : null}
      <AllowListRows
        isPending={sandboxes.isPending && listed.length === 0}
        isError={sandboxes.isError && listed.length === 0}
        error={sandboxes.error}
        retry={() => void sandboxes.refetch()}
        retrying={sandboxes.isFetching}
        plural="Sandboxes"
        empty={
          listed.length > 0
            ? null
            : search.trim() === ''
              ? 'No existing callers or sandboxes to allow.'
              : `No sandbox matches “${search.trim()}”.`
        }
      >
        {pageOf(listed, shownPage).map((sandbox) => (
          <SandboxCallerRow
            key={sandbox.id}
            sandbox={sandbox}
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
        page={shownPage}
        total={listed.length}
        onPageChange={setPage}
        ariaLabel="Sandbox pages"
      />
    </AllowListSection>
  )
}

type SandboxCaller = Pick<SandboxInner, 'id' | 'name' | 'status'>

/** The sandbox name, or its id when it has no other name. */
function sandboxName(sandbox: SandboxCaller): string {
  return sandbox.name.trim() === '' ? sandbox.id : sandbox.name
}

function SandboxCallerRow({
  sandbox,
  projectId,
  targetName,
  environmentId,
  grants,
  lookup,
  blockReason,
  onChanged,
}: {
  sandbox: SandboxCaller
  projectId: number
  targetName: string
  environmentId?: number
  grants: InternalAccessSandboxGrantResponse[]
  lookup: GrantsLookup
  blockReason: string | null
  onChanged: () => void
}) {
  const name = sandboxName(sandbox)
  const state = callerState(
    grants,
    (grant) => grant.sandbox_id === sandbox.id,
    environmentId
  )
  const allowMutation = useAllowGrant({
    allow: async (body: CreateInternalAccessSandboxGrantRequest) =>
      createInternalAccessSandboxGrant({
        path: { project_id: projectId },
        body,
        throwOnError: true,
      }),
    onChanged,
    allowed: `Sandbox ${name} can now call ${targetName}`,
  })
  const [confirming, setConfirming] = useState(false)
  const removeMutation = useMutation({
    mutationFn: async () => {
      for (const grant of state.removable) {
        await deleteInternalAccessSandboxGrant({
          path: { project_id: projectId, grant_id: grant.id },
          throwOnError: true,
        })
      }
    },
    ...removalFeedback(
      onChanged,
      `Sandbox ${name} can no longer call ${targetName}`
    ),
  })
  const detail =
    namedSandbox(sandbox.name, sandbox.id) === sandbox.id
      ? sandbox.status
      : `${sandbox.id} · ${sandbox.status}`
  return (
    <>
      <AllowRow
        icon={<Box className="h-5 w-5 text-muted-foreground" aria-hidden />}
        name={name}
        detail={detail}
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
          allowMutation.mutate(oneClickSandboxGrant(sandbox.id, environmentId))
        }
        onRemove={() => setConfirming(true)}
        limits={
          <GrantLimitsPopover
            name={name}
            targetProjectId={projectId}
            targetName={targetName}
            fixedTargetEnvironmentId={environmentId}
            disabled={lookup.state !== 'ready' || allowMutation.isPending}
            pending={allowMutation.isPending}
            onSubmit={(limits) =>
              allowMutation.mutateAsync(limitedSandboxGrant(sandbox.id, limits))
            }
          />
        }
      />
      <ConfirmDeleteDialog
        open={confirming}
        onOpenChange={setConfirming}
        resourceType="internal access"
        resourceName={name}
        verb="Remove"
        irreversible={false}
        consequences={`Calls from sandbox ${name} to ${targetName} get 403 again within seconds.`}
        isPending={removeMutation.isPending}
        onConfirm={() => {
          setConfirming(false)
          removeMutation.mutate()
        }}
      />
    </>
  )
}
