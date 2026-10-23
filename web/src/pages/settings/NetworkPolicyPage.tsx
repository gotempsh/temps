// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from 'react'
import {
  keepPreviousData,
  useMutation,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query'
import { Network, Plus, Search, Trash2, X } from 'lucide-react'
import { Link, useSearchParams } from 'react-router'
import { toast } from 'sonner'
import { CreateRuleDialog } from '@/components/network-policy/CreateRuleDialog'
import { SubjectPolicyPanel } from '@/components/network-policy/SubjectPolicyPanel'
import { PageHeader } from '@/components/layout/PageContainer'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { EmptyState } from '@/components/ui/empty-state'
import { Input } from '@/components/ui/input'
import { ReadFailure } from '@/components/ui/read-failure'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useIsInstanceAdmin } from '@/hooks/useIsInstanceAdmin'
import { usePageTitle } from '@/hooks/usePageTitle'
import { problemDetail } from '@/lib/api-problem'
import {
  deleteNetworkPolicyRule,
  describePorts,
  describeSubject,
  listNetworkPolicyRules,
  listNodeNetworkPolicyModes,
  networkPolicyKeys,
  ruleDeleteBlockedReason,
  SUBJECT_KINDS,
  type ListRulesParams,
  type NetworkPolicyRule,
  type RuleOrigin,
  type SubjectKind,
} from '@/lib/network-policy'
import { isForbiddenRead } from '@/lib/read-failure'

const PAGE_SIZE_OPTIONS = [20, 50, 100] as const
const ALL_ORIGINS = 'all'

function isSubjectKind(value: string | null): value is SubjectKind {
  return value !== null && (SUBJECT_KINDS as readonly string[]).includes(value)
}

function isOrigin(value: string | null): value is RuleOrigin {
  return value === 'link' || value === 'operator' || value === 'system'
}

function positiveInt(value: string | null, fallback: number): number {
  const parsed = Number(value)
  return Number.isInteger(parsed) && parsed > 0 ? parsed : fallback
}

function modeVariant(mode: string) {
  if (mode === 'enforce') return 'success-subtle' as const
  if (mode === 'shadow') return 'warning-subtle' as const
  return 'neutral-subtle' as const
}

/** Which nodes enforce the policy, linking to each node's mode setting. */
function NodeModesSummary() {
  const query = useQuery({
    queryKey: networkPolicyKeys.nodes,
    queryFn: listNodeNetworkPolicyModes,
    meta: { errorTitle: 'Could not load the network policy modes' },
  })
  return (
    <div className="space-y-2 rounded-lg border bg-card p-4 text-card-foreground">
      <h2 className="text-base font-semibold">Where it applies</h2>
      <p className="text-sm text-muted-foreground">
        Rules take effect only on nodes in Enforce mode. Shadow computes and
        logs the policy without applying it. Change a node&apos;s mode on its
        page.
      </p>
      {query.isPending ? (
        <Skeleton className="h-6 w-2/3" />
      ) : query.isError ? (
        <ReadFailure
          resource="Node modes"
          error={query.error}
          embedded
          onRetry={
            isForbiddenRead(query.error)
              ? undefined
              : () => void query.refetch()
          }
          retrying={query.isFetching}
        />
      ) : (
        <ul className="flex flex-wrap gap-2">
          {query.data.nodes.map((node) => (
            <li key={node.node_id}>
              <Link
                to={`/settings/nodes/${node.node_id}`}
                className="inline-flex items-center gap-2 rounded-md border px-2.5 py-1 text-sm hover:bg-muted/50"
              >
                <span className="font-medium">{node.name}</span>
                <Badge variant={modeVariant(node.mode)}>{node.mode}</Badge>
              </Link>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

function RuleRows({
  rules,
  canEdit,
  onDelete,
}: {
  rules: NetworkPolicyRule[]
  canEdit: boolean
  onDelete: (rule: NetworkPolicyRule) => void
}) {
  return (
    <>
      {rules.map((rule) => {
        const blocked = ruleDeleteBlockedReason(rule)
        return (
          <TableRow key={rule.id}>
            <TableCell className="font-mono text-xs">#{rule.id}</TableCell>
            <TableCell>
              <div className="font-medium">
                {describeSubject(rule.subject_kind, rule.subject_id)}
              </div>
              {rule.description ? (
                <div className="max-w-[240px] truncate text-xs text-muted-foreground">
                  {rule.description}
                </div>
              ) : null}
            </TableCell>
            <TableCell>
              <Badge
                variant={
                  rule.action === 'allow'
                    ? 'success-subtle'
                    : 'destructive-subtle'
                }
              >
                {rule.action}
              </Badge>{' '}
              <span className="text-sm text-muted-foreground">
                {rule.direction}
              </span>
            </TableCell>
            <TableCell className="min-w-0">
              <span className="text-xs text-muted-foreground">
                {rule.peer_kind}
              </span>{' '}
              <span className="break-all font-mono text-xs">
                {rule.peer_ref}
              </span>
            </TableCell>
            <TableCell className="whitespace-nowrap text-sm">
              {describePorts(rule)}
            </TableCell>
            <TableCell>
              <Badge variant="outline">{rule.origin}</Badge>
            </TableCell>
            <TableCell className="text-right">
              {!canEdit ? null : blocked ? (
                <span className="text-xs text-muted-foreground" title={blocked}>
                  Managed by a link
                </span>
              ) : (
                <Button
                  variant="ghost"
                  size="icon"
                  aria-label={`Delete rule #${rule.id}`}
                  onClick={() => onDelete(rule)}
                >
                  <Trash2 className="h-4 w-4" />
                </Button>
              )}
            </TableCell>
          </TableRow>
        )
      })}
    </>
  )
}

function SkeletonRows() {
  return (
    <>
      {[0, 1, 2].map((row) => (
        <TableRow key={row}>
          {[0, 1, 2, 3, 4, 5, 6].map((cell) => (
            <TableCell key={cell}>
              <Skeleton className="h-4 w-full max-w-[120px]" />
            </TableCell>
          ))}
        </TableRow>
      ))}
    </>
  )
}

/**
 * Instance-wide network policy rules (ADR-052 §4, ADR-055 §9): what each
 * workload may reach, with a per-workload effective view.
 */
export function NetworkPolicyPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  usePageTitle('Network Policy')
  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Network Policy' },
    ])
  }, [setBreadcrumbs])

  const canEdit = useIsInstanceAdmin()
  const queryClient = useQueryClient()
  const [searchParams, setSearchParams] = useSearchParams()
  const page = positiveInt(searchParams.get('page'), 1)
  const pageSize = Math.min(positiveInt(searchParams.get('page_size'), 20), 100)
  const originParam = searchParams.get('origin')
  const origin = isOrigin(originParam) ? originParam : undefined
  const kindParam = searchParams.get('kind')
  const subjectKind = isSubjectKind(kindParam) ? kindParam : undefined
  const idParam = searchParams.get('id')
  const subjectId =
    subjectKind && idParam !== null ? positiveInt(idParam, 0) : 0
  const subject =
    subjectKind && subjectId > 0
      ? { kind: subjectKind, id: subjectId }
      : undefined

  const [draftKind, setDraftKind] = useState<SubjectKind>(
    subjectKind ?? 'sandbox'
  )
  const [draftId, setDraftId] = useState(subject ? String(subject.id) : '')
  const [createOpen, setCreateOpen] = useState(false)
  const [toDelete, setToDelete] = useState<NetworkPolicyRule | null>(null)

  const updateParams = (change: (next: URLSearchParams) => void) =>
    setSearchParams(
      (prev) => {
        const next = new URLSearchParams(prev)
        change(next)
        return next
      },
      { replace: true }
    )

  const params: ListRulesParams = {
    page,
    page_size: pageSize,
    origin,
    subject_kind: subject?.kind,
    subject_id: subject?.id,
  }
  const rulesQuery = useQuery({
    queryKey: networkPolicyKeys.rules(params),
    queryFn: () => listNetworkPolicyRules(params),
    placeholderData: keepPreviousData,
    meta: { errorTitle: 'Could not load network policy rules' },
  })

  const remove = useMutation({
    mutationFn: (rule: NetworkPolicyRule) => deleteNetworkPolicyRule(rule.id),
    meta: { errorTitle: 'Could not delete the rule' },
    onSuccess: async (_, rule) => {
      toast.success(`Rule #${rule.id} deleted`)
      setToDelete(null)
      const remaining = (rulesQuery.data?.items.length ?? 1) - 1
      if (remaining === 0 && page > 1) {
        updateParams((next) => next.set('page', String(page - 1)))
      }
      await queryClient.invalidateQueries({ queryKey: networkPolicyKeys.all })
    },
    onError: (error, rule) => {
      toast.error(`Could not delete rule #${rule.id}`, {
        description: problemDetail(error, 'The server refused the delete.'),
      })
    },
  })

  const total = rulesQuery.data?.total ?? 0
  const totalPages = Math.max(1, Math.ceil(total / pageSize))
  const filtered = origin !== undefined || subject !== undefined

  return (
    <div className="w-full min-w-0 space-y-6">
      <PageHeader
        title="Network policy"
        description="What each workload may reach: allow and deny rules per sandbox, service, function, job or deployment, plus the links that create them."
        actions={
          canEdit ? (
            <Button onClick={() => setCreateOpen(true)}>
              <Plus className="mr-2 h-4 w-4" />
              Add rule
            </Button>
          ) : null
        }
      />

      <NodeModesSummary />

      <form
        className="flex flex-col gap-2 sm:flex-row sm:flex-wrap sm:items-end"
        onSubmit={(event) => {
          event.preventDefault()
          const id = positiveInt(draftId, 0)
          if (id === 0) {
            toast.error('Enter a workload ID', {
              description:
                draftKind === 'deployment'
                  ? 'For a deployment, use the environment ID.'
                  : `Use the ${draftKind}'s numeric ID.`,
            })
            return
          }
          updateParams((next) => {
            next.set('kind', draftKind)
            next.set('id', String(id))
            next.delete('page')
          })
        }}
      >
        <div className="w-full space-y-1.5 sm:w-[180px]">
          <label htmlFor="np-kind" className="text-sm font-medium">
            Workload kind
          </label>
          <Select
            value={draftKind}
            onValueChange={(value) => setDraftKind(value as SubjectKind)}
          >
            <SelectTrigger id="np-kind">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {SUBJECT_KINDS.map((kind) => (
                <SelectItem key={kind} value={kind}>
                  {kind}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <div className="w-full space-y-1.5 sm:w-[160px]">
          <label htmlFor="np-id" className="text-sm font-medium">
            {draftKind === 'deployment' ? 'Environment ID' : 'Workload ID'}
          </label>
          <Input
            id="np-id"
            inputMode="numeric"
            placeholder="42"
            value={draftId}
            onChange={(event) => setDraftId(event.target.value)}
          />
        </div>
        <Button type="submit" variant="outline">
          <Search className="mr-2 h-4 w-4" />
          Inspect workload
        </Button>
        <div className="w-full space-y-1.5 sm:ml-auto sm:w-[180px]">
          <label htmlFor="np-origin" className="text-sm font-medium">
            Origin
          </label>
          <Select
            value={origin ?? ALL_ORIGINS}
            onValueChange={(value) =>
              updateParams((next) => {
                if (value === ALL_ORIGINS) next.delete('origin')
                else next.set('origin', value)
                next.delete('page')
              })
            }
          >
            <SelectTrigger id="np-origin">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={ALL_ORIGINS}>All origins</SelectItem>
              <SelectItem value="operator">Operator</SelectItem>
              <SelectItem value="link">Link</SelectItem>
              <SelectItem value="system">System</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </form>

      {subject ? (
        <div className="space-y-2">
          <div className="flex justify-end">
            <Button
              variant="ghost"
              size="sm"
              onClick={() =>
                updateParams((next) => {
                  next.delete('kind')
                  next.delete('id')
                  next.delete('page')
                })
              }
            >
              <X className="mr-2 h-4 w-4" />
              Show every workload
            </Button>
          </div>
          <SubjectPolicyPanel
            key={`${subject.kind}-${subject.id}`}
            subjectKind={subject.kind}
            subjectId={subject.id}
            canEdit={canEdit}
          />
        </div>
      ) : null}

      <div className="rounded-lg border bg-card text-card-foreground">
        {rulesQuery.isError && !rulesQuery.data ? (
          <div className="p-4">
            <ReadFailure
              resource="Network policy rules"
              error={rulesQuery.error}
              embedded
              onRetry={
                isForbiddenRead(rulesQuery.error)
                  ? undefined
                  : () => void rulesQuery.refetch()
              }
              retrying={rulesQuery.isFetching}
            />
          </div>
        ) : !rulesQuery.isPending && total === 0 ? (
          <EmptyState
            size="compact"
            icon={Network}
            title={
              filtered ? 'No matching rules' : 'No network policy rules yet'
            }
            description={
              filtered
                ? 'No rule matches the selected workload or origin. Clear the filters to see every rule.'
                : 'Without rules, deployments, services, functions and jobs reach anything, and sandboxes reach only what a link allows.'
            }
            action={
              canEdit && !filtered ? (
                <Button onClick={() => setCreateOpen(true)}>
                  <Plus className="mr-2 h-4 w-4" />
                  Add rule
                </Button>
              ) : undefined
            }
          />
        ) : (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Rule</TableHead>
                <TableHead>Workload</TableHead>
                <TableHead>Action</TableHead>
                <TableHead>Peer</TableHead>
                <TableHead>Ports</TableHead>
                <TableHead>Origin</TableHead>
                <TableHead className="text-right">
                  <span className="sr-only">Actions</span>
                </TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {rulesQuery.isPending ? (
                <SkeletonRows />
              ) : (
                <RuleRows
                  rules={rulesQuery.data?.items ?? []}
                  canEdit={canEdit}
                  onDelete={setToDelete}
                />
              )}
            </TableBody>
          </Table>
        )}
      </div>

      {total > 0 ? (
        <ResponsivePagination
          page={page}
          pageSize={pageSize}
          total={total}
          totalPages={totalPages}
          pageSizeOptions={PAGE_SIZE_OPTIONS}
          onPageChange={(next) =>
            updateParams((params) => params.set('page', String(next)))
          }
          onPageSizeChange={(size) =>
            updateParams((params) => {
              params.set('page_size', String(size))
              params.delete('page')
            })
          }
          ariaLabel="Network policy rules pagination"
        />
      ) : null}

      <CreateRuleDialog
        key={subject ? `${subject.kind}-${subject.id}` : 'any'}
        open={createOpen}
        onOpenChange={setCreateOpen}
        subject={
          subject
            ? { subject_kind: subject.kind, subject_id: String(subject.id) }
            : undefined
        }
      />
      <ConfirmDeleteDialog
        open={toDelete !== null}
        onOpenChange={(open) => {
          if (!open) setToDelete(null)
        }}
        resourceType="network policy rule"
        resourceName={
          toDelete
            ? `#${toDelete.id} (${describeSubject(toDelete.subject_kind, toDelete.subject_id)} → ${toDelete.peer_ref})`
            : ''
        }
        consequences={
          toDelete
            ? `On nodes in Enforce mode, ${describeSubject(toDelete.subject_kind, toDelete.subject_id)} ${
                toDelete.action === 'allow'
                  ? `loses its ${toDelete.direction} access to ${toDelete.peer_ref} unless another rule allows it.`
                  : `is no longer denied ${toDelete.direction} traffic to ${toDelete.peer_ref}.`
              }`
            : undefined
        }
        isPending={remove.isPending}
        onConfirm={() => {
          if (toDelete) remove.mutate(toDelete)
        }}
      />
    </div>
  )
}
