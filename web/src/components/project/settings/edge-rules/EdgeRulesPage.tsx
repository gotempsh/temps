// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ProjectResponse } from '@/api/client'
import { PageHeader } from '@/components/layout/PageContainer'
import { Button } from '@/components/ui/button'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { EmptyState } from '@/components/ui/empty-state'
import { ReadFailure } from '@/components/ui/read-failure'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { Button as BusyButton, RecordLink, Status } from '@temps-sdk/ds'
import { useQuery, type UseQueryResult } from '@tanstack/react-query'
import { formatDistanceToNow } from 'date-fns'
import {
  ArrowDown,
  ArrowUp,
  ChevronDown,
  FlaskConical,
  ListFilter,
  MoreHorizontal,
  Plus,
} from 'lucide-react'
import { useEffect, useRef, useState, type ReactNode } from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { ActionIcon } from './action-icons'
import { describeAction, describeExpression } from './expression'
import { LIMITS } from './fields'
import {
  edgeRulesPath,
  environmentLabel,
  useProjectEnvironments,
} from './shared'
import {
  edgeRulesOptions,
  useCreateEdgeRule,
  useDeleteEdgeRule,
  useMoveEdgeRule,
  useUpdateEdgeRule,
} from './store'
import { RULE_TEMPLATES } from './templates'
import { TestRequestSheet } from './TestRequestSheet'
import type { EdgeRule } from './types'

interface EdgeRulesPageProps {
  project: ProjectResponse
}

export function EdgeRulesPage({ project }: EdgeRulesPageProps) {
  const { setBreadcrumbs } = useBreadcrumbs()
  const base = edgeRulesPath(project.slug)
  const rulesQuery = useQuery(edgeRulesOptions(project.id))
  const { environments } = useProjectEnvironments(project.id)
  const createRule = useCreateEdgeRule(project.id)
  const updateRule = useUpdateEdgeRule(project.id)
  const deleteRule = useDeleteEdgeRule(project.id)
  const moveRule = useMoveEdgeRule(project.id)
  const [testOpen, setTestOpen] = useState(false)
  const [pendingDelete, setPendingDelete] = useState<EdgeRule | null>(null)
  // The row's "More actions" button. The menu item that opened the dialog is
  // gone by the time it closes, so focus goes back to the button instead.
  const deleteOpenerRef = useRef<HTMLElement | null>(null)

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Projects', href: '/projects' },
      { label: project.slug, href: `/projects/${project.slug}/project` },
      { label: 'Edge rules' },
    ])
  }, [project.slug, setBreadcrumbs])

  const rules = rulesQuery.data ?? []
  const atLimit = rules.length >= LIMITS.rulesPerProject
  const busyRuleId =
    (updateRule.isPending && updateRule.variables?.ruleId) ||
    (moveRule.isPending && moveRule.variables?.ruleId) ||
    null

  const addTemplate = (templateId: string) => {
    const template = RULE_TEMPLATES.find((t) => t.id === templateId)
    if (!template) return
    createRule.mutate(template.build(), {
      onSuccess: (rule) =>
        toast.success(`Added "${rule.name}" in log-only mode`, {
          description:
            'It records matches without acting on them. Switch it to enforce when you are happy with it.',
        }),
    })
  }

  const duplicate = (rule: EdgeRule) => {
    const {
      id: _id,
      revision: _rev,
      createdAt: _c,
      updatedAt: _u,
      hits: _h,
      lastHitAt: _l,
      ...draft
    } = rule
    createRule.mutate(
      {
        ...draft,
        name: `Copy of ${rule.name}`.slice(0, LIMITS.nameLength),
        enabled: false,
      },
      {
        onSuccess: (copy) =>
          toast.success(
            `Added "${copy.name}", disabled, at the end of the list`
          ),
      }
    )
  }

  const addAction = atLimit ? null : (
    <Button asChild>
      <Link to={`${base}/new`}>
        <Plus />
        Add rule
      </Link>
    </Button>
  )

  // One header control. The empty state carries its own call to action, so
  // the header stays bare until there is a list to add to.
  const headerAction =
    atLimit || rules.length === 0 ? null : (
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button disabled={createRule.isPending}>
            <Plus />
            Add rule
            <ChevronDown className="opacity-70" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-80">
          <DropdownMenuItem asChild>
            <Link to={`${base}/new`}>
              <Plus />
              Blank rule
            </Link>
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuLabel className="text-xs font-normal text-muted-foreground">
            From an example (log only)
          </DropdownMenuLabel>
          {RULE_TEMPLATES.map((template) => (
            <DropdownMenuItem
              key={template.id}
              onSelect={() => addTemplate(template.id)}
            >
              <ActionIcon type={template.build().action.type} />
              {template.title}
            </DropdownMenuItem>
          ))}
        </DropdownMenuContent>
      </DropdownMenu>
    )

  return (
    <div className="w-full min-w-0 space-y-6">
      <PageHeader
        title="Edge rules"
        description="Checked top to bottom before a request reaches your app. The first matching rule decides."
        actions={headerAction}
      />

      {/* Mounted for the page's whole life so screen readers announce the
          change; a live region that appears with its text is often missed. */}
      <p role="status" className="sr-only">
        {rulesQuery.isPending ? 'Loading edge rules' : ''}
      </p>

      <div className="rounded-lg border bg-card text-card-foreground">
        {rules.length > 0 && (
          <div className="flex items-center justify-between gap-2 border-b px-4 py-2">
            <p className="text-sm text-muted-foreground">
              {rules.length} {rules.length === 1 ? 'rule' : 'rules'}
            </p>
            <Button variant="ghost" size="sm" onClick={() => setTestOpen(true)}>
              <FlaskConical />
              Test a request
            </Button>
          </div>
        )}
        <RulesCollection
          query={rulesQuery}
          rules={rules}
          base={base}
          environments={environments}
          busyRuleId={busyRuleId}
          moving={moveRule.isPending}
          onToggle={(rule, enabled) =>
            updateRule.mutate({
              ruleId: rule.id,
              revision: rule.revision,
              patch: { enabled },
            })
          }
          onSetMode={(rule, mode) =>
            updateRule.mutate(
              { ruleId: rule.id, revision: rule.revision, patch: { mode } },
              {
                onSuccess: (updated) =>
                  toast.success(
                    mode === 'enforce'
                      ? `"${updated.name}" is now enforced`
                      : `"${updated.name}" now only logs matches`
                  ),
              }
            )
          }
          onMove={(rule, toIndex) =>
            moveRule.mutate({ ruleId: rule.id, toIndex })
          }
          onDuplicate={duplicate}
          onDelete={(rule, opener) => {
            deleteOpenerRef.current = opener
            setPendingDelete(rule)
          }}
          onAddTemplate={addTemplate}
          addingTemplate={createRule.isPending}
          addAction={addAction}
        />
      </div>

      {atLimit && (
        <p className="text-sm text-muted-foreground">
          This project has {LIMITS.rulesPerProject} rules, the maximum. Combine
          or delete rules to add more.
        </p>
      )}

      <p className="max-w-3xl text-sm text-muted-foreground">
        Runs after IP restrictions and before the rest of{' '}
        <Link
          to={`/projects/${project.slug}/settings/security`}
          className="text-foreground underline underline-offset-4"
        >
          Protection
        </Link>
        . Certificate checks and Temps&apos; challenge pages always bypass it.
      </p>

      <TestRequestSheet
        open={testOpen}
        onOpenChange={setTestOpen}
        rules={rules}
        environments={environments}
      />

      <ConfirmDeleteDialog
        open={pendingDelete !== null}
        onOpenChange={(open) => !open && setPendingDelete(null)}
        resourceType="edge rule"
        resourceName={pendingDelete?.name ?? ''}
        consequences="Requests it matches will be handled by the remaining rules, or reach your app if no other rule matches."
        isPending={deleteRule.isPending}
        returnFocusRef={deleteOpenerRef}
        onConfirm={() => {
          if (!pendingDelete) return
          const name = pendingDelete.name
          deleteRule.mutate(pendingDelete.id, {
            onSuccess: () => {
              setPendingDelete(null)
              toast.success(`Deleted "${name}"`)
            },
          })
        }}
      />
    </div>
  )
}

interface RulesCollectionProps {
  query: UseQueryResult<EdgeRule[]>
  rules: EdgeRule[]
  base: string
  environments: Parameters<typeof environmentLabel>[0]
  busyRuleId: string | null
  /** A reorder is in flight; every move waits for it, since indexes shift. */
  moving: boolean
  onToggle: (rule: EdgeRule, enabled: boolean) => void
  onSetMode: (rule: EdgeRule, mode: EdgeRule['mode']) => void
  onMove: (rule: EdgeRule, toIndex: number) => void
  onDuplicate: (rule: EdgeRule) => void
  onDelete: (rule: EdgeRule, opener: HTMLElement | null) => void
  onAddTemplate: (templateId: string) => void
  addingTemplate: boolean
  addAction: ReactNode
}

function RulesCollection(props: RulesCollectionProps) {
  const { query, rules } = props
  if (query.isPending) return <RulesSkeleton />
  if (query.isError && rules.length === 0) {
    return (
      <ReadFailure
        resource="edge rules"
        error={query.error}
        onRetry={() => void query.refetch()}
        retrying={query.isFetching}
        embedded
      />
    )
  }
  if (rules.length === 0) return <RulesEmpty {...props} />
  return <RulesTable {...props} />
}

function RulesSkeleton() {
  return (
    // Decorative: the page's status region announces the loading state.
    <div className="divide-y" aria-hidden="true">
      {[0, 1, 2].map((i) => (
        <div key={i} className="flex items-center gap-4 px-4 py-4">
          <Skeleton className="h-4 w-6" />
          <div className="flex-1 space-y-2">
            <Skeleton className="h-4 w-48" />
            <Skeleton className="h-3 w-80 max-w-full" />
          </div>
          <Skeleton className="h-5 w-20" />
          <Skeleton className="h-6 w-10" />
        </div>
      ))}
    </div>
  )
}

function RulesEmpty({
  onAddTemplate,
  addingTemplate,
  addAction,
}: RulesCollectionProps) {
  return (
    <div className="divide-y">
      <EmptyState
        size="compact"
        icon={ListFilter}
        title="No edge rules yet"
        description="Block probes, restrict paths, challenge bots or redirect before requests reach your app."
        action={addAction}
      />
      <div className="px-4 py-3">
        <h2 className="text-sm font-semibold">Start from an example</h2>
        <p className="text-xs text-muted-foreground">
          Added in log-only mode, so nothing is blocked until you enforce it.
        </p>
      </div>
      <ul className="divide-y">
        {RULE_TEMPLATES.map((template) => (
          <li
            key={template.id}
            className="flex flex-col gap-2 px-4 py-3 sm:flex-row sm:items-center sm:justify-between"
          >
            <div className="flex min-w-0 items-start gap-3">
              <span className="mt-0.5 flex size-7 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground">
                <ActionIcon type={template.build().action.type} />
              </span>
              <div className="min-w-0">
                <p className="text-sm font-medium">{template.title}</p>
                <p className="text-sm text-muted-foreground">
                  {template.description}
                </p>
              </div>
            </div>
            <BusyButton
              variant="outline"
              size="sm"
              busy={addingTemplate}
              onClick={() => onAddTemplate(template.id)}
              aria-label={`Add example: ${template.title}`}
            >
              Add
            </BusyButton>
          </li>
        ))}
      </ul>
    </div>
  )
}

/** How often the rule matched: the evidence for switching log-only to enforce. */
function RuleHits({ rule }: { rule: EdgeRule }) {
  const hits = rule.hits ?? 0
  const last = rule.lastHitAt ? new Date(rule.lastHitAt) : null
  return (
    <p className="mt-0.5 text-xs text-muted-foreground tabular-nums">
      {hits === 0
        ? 'No matches yet'
        : `${hits.toLocaleString()} ${hits === 1 ? 'match' : 'matches'}`}
      {last && !Number.isNaN(last.getTime()) && (
        <span title={last.toISOString()}>
          {' '}
          · {formatDistanceToNow(last, { addSuffix: true })}
        </span>
      )}
    </p>
  )
}

function RulesTable({
  rules,
  base,
  environments,
  busyRuleId,
  moving,
  onToggle,
  onSetMode,
  onMove,
  onDuplicate,
  onDelete,
}: RulesCollectionProps) {
  const menuTriggers = useRef(new Map<string, HTMLButtonElement>())
  return (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead className="w-12">#</TableHead>
          <TableHead>Rule</TableHead>
          <TableHead className="hidden md:table-cell">Applies to</TableHead>
          <TableHead>Action</TableHead>
          <TableHead>Mode</TableHead>
          <TableHead>Enabled</TableHead>
          <TableHead className="text-right">
            <span className="sr-only">Actions</span>
          </TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {rules.map((rule, index) => {
          const busy = busyRuleId === rule.id
          return (
            <TableRow
              key={rule.id}
              className={rule.enabled ? undefined : 'text-muted-foreground'}
            >
              <TableCell className="tabular-nums text-muted-foreground">
                {index + 1}
              </TableCell>
              <TableCell className="min-w-[240px]">
                <RecordLink
                  to={`${base}/${rule.id}`}
                  aria-label={`Edit ${rule.name}`}
                >
                  {/* RecordLink breaks anywhere; names should wrap between words. */}
                  <span className="[overflow-wrap:anywhere] [word-break:normal]">
                    {rule.name}
                  </span>
                </RecordLink>
                <p className="line-clamp-2 text-xs text-muted-foreground">
                  When {describeExpression(rule.expression)}
                </p>
              </TableCell>
              <TableCell className="hidden whitespace-nowrap md:table-cell">
                {environmentLabel(environments, rule.environmentId)}
              </TableCell>
              <TableCell className="whitespace-nowrap">
                <span className="inline-flex items-center gap-2">
                  <ActionIcon
                    type={rule.action.type}
                    className="text-muted-foreground"
                  />
                  {describeAction(rule.action)}
                </span>
              </TableCell>
              <TableCell className="whitespace-nowrap">
                {rule.mode === 'enforce' ? (
                  <Status tone="ok" label="Enforced" variant="dot" />
                ) : (
                  <Status tone="idle" label="Log only" variant="dot" />
                )}
                <RuleHits rule={rule} />
              </TableCell>
              <TableCell>
                <Switch
                  checked={rule.enabled}
                  disabled={busy}
                  aria-label={`${rule.enabled ? 'Disable' : 'Enable'} ${rule.name}`}
                  onCheckedChange={(checked) => onToggle(rule, checked)}
                />
              </TableCell>
              <TableCell className="text-right">
                <div className="flex items-center justify-end gap-1">
                  <Button
                    variant="ghost"
                    size="icon"
                    className="size-9"
                    disabled={busy || moving || index === 0}
                    aria-label={`Move ${rule.name} up`}
                    onClick={() => onMove(rule, index - 1)}
                  >
                    <ArrowUp />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon"
                    className="size-9"
                    disabled={busy || moving || index === rules.length - 1}
                    aria-label={`Move ${rule.name} down`}
                    onClick={() => onMove(rule, index + 1)}
                  >
                    <ArrowDown />
                  </Button>
                  <DropdownMenu>
                    <DropdownMenuTrigger asChild>
                      <Button
                        variant="ghost"
                        size="icon"
                        className="size-9"
                        aria-label={`More actions for ${rule.name}`}
                        ref={(node) => {
                          if (node) menuTriggers.current.set(rule.id, node)
                          else menuTriggers.current.delete(rule.id)
                        }}
                      >
                        <MoreHorizontal />
                      </Button>
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end">
                      <DropdownMenuItem asChild>
                        <Link to={`${base}/${rule.id}`}>Edit</Link>
                      </DropdownMenuItem>
                      <DropdownMenuItem
                        onSelect={() =>
                          onSetMode(
                            rule,
                            rule.mode === 'enforce' ? 'log_only' : 'enforce'
                          )
                        }
                      >
                        {rule.mode === 'enforce'
                          ? 'Switch to log only'
                          : 'Enforce'}
                      </DropdownMenuItem>
                      <DropdownMenuItem onSelect={() => onDuplicate(rule)}>
                        Duplicate
                      </DropdownMenuItem>
                      <DropdownMenuSeparator />
                      <DropdownMenuItem
                        className="text-destructive focus:text-destructive"
                        onSelect={() =>
                          onDelete(
                            rule,
                            menuTriggers.current.get(rule.id) ?? null
                          )
                        }
                      >
                        Delete
                      </DropdownMenuItem>
                    </DropdownMenuContent>
                  </DropdownMenu>
                </div>
              </TableCell>
            </TableRow>
          )
        })}
      </TableBody>
    </Table>
  )
}
