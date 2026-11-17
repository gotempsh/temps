// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The pieces every internal access list shares (ADR 101, ADR 105): a
 * searchable, paged list of callers or targets, each row allowed in one
 * click with the default scope, or with a narrower scope and a reason from
 * its "Limit…" popover. Whether a row is allowed comes from the grants
 * themselves; while that lookup is loading, failed or incomplete, every
 * Allow waits, so an allowed row never offers "Allow".
 */
import { zodResolver } from '@hookform/resolvers/zod'
import { useQuery } from '@tanstack/react-query'
import type { LucideIcon } from 'lucide-react'
import { Search, SlidersHorizontal, Trash2 } from 'lucide-react'
import { useState, type ReactNode } from 'react'
import { useForm } from 'react-hook-form'
import { getEnvironmentsOptions } from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { problemDetail } from '@/lib/api-problem'
import {
  GRANTS_PAGE_SIZE,
  MAX_GRANT_DESCRIPTION_LENGTH,
  defaultGrantLimits,
  grantLimitsSchema,
  scopesSummary,
  type CallerState,
  type GrantLimits,
  type GrantScope,
} from './internal-access'
import type { GrantsLookup } from './internal-access-queries'

export function AllowListSection({
  title,
  icon: Icon,
  search,
  onSearchChange,
  searchLabel,
  note,
  children,
}: {
  title: string
  icon: LucideIcon
  search: string
  onSearchChange: (value: string) => void
  searchLabel: string
  /** One short line under the heading, when the list needs it. */
  note?: ReactNode
  children: ReactNode
}) {
  return (
    <section className="space-y-3" aria-label={title}>
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <h3 className="flex items-center gap-2 text-sm font-medium">
          <Icon className="h-4 w-4" aria-hidden />
          {title}
        </h3>
        <div className="relative w-full sm:w-[240px]">
          <Search
            className="pointer-events-none absolute left-2.5 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground"
            aria-hidden
          />
          <Input
            type="search"
            value={search}
            onChange={(event) => onSearchChange(event.target.value)}
            placeholder="Search…"
            aria-label={searchLabel}
            className="pl-8"
          />
        </div>
      </div>
      {note ? <p className="text-xs text-muted-foreground">{note}</p> : null}
      {children}
    </section>
  )
}

/** Shown above the rows when Allow waits for the grants lookup. */
export function LookupNotice({
  lookup,
  plural,
}: {
  lookup: GrantsLookup
  plural: string
}) {
  if (lookup.state === 'failed') {
    return (
      <ErrorAlert
        title={`Could not check which ${plural} are allowed`}
        description={`${problemDetail(lookup.error, 'The grants could not be loaded.')} Allowing is paused until this check succeeds.`}
        retry={lookup.retry}
        retrying={lookup.retrying}
      />
    )
  }
  if (lookup.state === 'incomplete') {
    return (
      <p className="rounded-md border border-dashed p-3 text-sm text-muted-foreground">
        This project holds more grants than one lookup returns, so which{' '}
        {plural} are allowed cannot be shown. Allowing is paused.
      </p>
    )
  }
  return null
}

export function AllowListRows({
  isPending,
  isError,
  error,
  retry,
  retrying,
  plural,
  empty,
  children,
}: {
  isPending: boolean
  isError: boolean
  error: unknown
  retry: () => void
  retrying: boolean
  plural: string
  /** Shown when there is nothing to list. */
  empty: ReactNode | null
  children: ReactNode
}) {
  if (isError) {
    return (
      <ErrorAlert
        title={`Could not load ${plural}`}
        description={problemDetail(error, `The ${plural} could not be loaded.`)}
        retry={retry}
        retrying={retrying}
      />
    )
  }
  if (isPending) {
    return (
      <div className="divide-y rounded-md border" aria-busy="true">
        {[0, 1, 2].map((row) => (
          <div key={row} className="flex items-center gap-3 p-3">
            <Skeleton className="h-6 w-6 rounded-md" />
            <Skeleton className="h-4 w-40" />
            <Skeleton className="ml-auto h-8 w-20" />
          </div>
        ))}
      </div>
    )
  }
  if (empty !== null) {
    return (
      <p className="rounded-md border border-dashed p-3 text-sm text-muted-foreground">
        {empty}
      </p>
    )
  }
  return (
    <ul className="divide-y rounded-md border" aria-label={plural}>
      {children}
    </ul>
  )
}

export function AllowListPager({
  page,
  total,
  onPageChange,
  ariaLabel,
}: {
  page: number
  total: number
  onPageChange: (page: number) => void
  ariaLabel: string
}) {
  const totalPages = Math.ceil(total / GRANTS_PAGE_SIZE)
  if (totalPages <= 1) return null
  return (
    <ResponsivePagination
      page={page}
      pageSize={GRANTS_PAGE_SIZE}
      total={total}
      totalPages={totalPages}
      onPageChange={onPageChange}
      ariaLabel={ariaLabel}
    />
  )
}

export function AllowRow({
  icon,
  name,
  detail,
  state,
  lookup,
  blockReason,
  allowing,
  removing,
  error,
  onAllow,
  onRemove,
  limits,
}: {
  icon: ReactNode
  name: string
  /** A second, quieter line: a sandbox's id and state. */
  detail?: string
  state: CallerState<GrantScope>
  lookup: GrantsLookup
  blockReason: string | null
  allowing: boolean
  removing: boolean
  /** The last Allow or Remove failure, said in place. */
  error: unknown
  onAllow: () => void
  /** Asks to remove; the row's module owns the confirmation. */
  onRemove: () => void
  /** The "Limit…" popover; shown while the row is not allowed. */
  limits: ReactNode
}) {
  const allowed = state.grants.length > 0
  // Until the grants are known, a row may already be allowed.
  const paused: RowPause =
    lookup.state === 'ready'
      ? false
      : lookup.state === 'pending'
        ? 'pending'
        : 'paused'
  const descriptions = state.grants
    .map((grant) => grant.description)
    .filter((description): description is string => !!description)
  return (
    <li className="flex flex-col gap-2 p-3 sm:flex-row sm:items-center sm:justify-between">
      <div className="flex min-w-0 items-start gap-3">
        <span className="mt-0.5 shrink-0">{icon}</span>
        <div className="min-w-0 space-y-1">
          <p className="text-sm font-medium [overflow-wrap:anywhere]">{name}</p>
          {detail ? (
            <p className="text-xs text-muted-foreground [overflow-wrap:anywhere]">
              {detail}
            </p>
          ) : null}
          {allowed && !paused ? (
            <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
              <Badge variant="success-subtle">Allowed</Badge>
              <span>{scopesSummary(state.grants)}</span>
              {state.inherited ? (
                <Badge variant="neutral-subtle">Project-wide</Badge>
              ) : null}
            </div>
          ) : null}
          {allowed && !paused && descriptions.length > 0 ? (
            <p className="text-xs text-muted-foreground [overflow-wrap:anywhere]">
              {descriptions.join(' · ')}
            </p>
          ) : null}
          {error ? (
            <p
              role="alert"
              className="text-xs text-destructive [overflow-wrap:anywhere]"
            >
              {problemDetail(error, 'The change was refused.')}
            </p>
          ) : null}
        </div>
      </div>
      <div className="flex shrink-0 items-center gap-2 sm:justify-end">
        <RowActions
          name={name}
          allowed={allowed}
          inherited={state.inherited}
          paused={paused}
          blockReason={blockReason}
          allowing={allowing}
          removing={removing}
          onAllow={onAllow}
          onRemove={onRemove}
          limits={limits}
        />
      </div>
    </li>
  )
}

/** Why Allow waits: the lookup is loading, or it failed or was incomplete. */
type RowPause = false | 'pending' | 'paused'

function RowActions({
  name,
  allowed,
  inherited,
  paused,
  blockReason,
  allowing,
  removing,
  onAllow,
  onRemove,
  limits,
}: {
  name: string
  allowed: boolean
  inherited: boolean
  paused: RowPause
  blockReason: string | null
  allowing: boolean
  removing: boolean
  onAllow: () => void
  onRemove: () => void
  limits: ReactNode
}) {
  if (blockReason !== null) {
    return (
      <span className="text-xs text-muted-foreground">
        {allowed ? 'Allowed · ' : ''}Needs projects:write
      </span>
    )
  }
  if (allowed && !paused) {
    if (inherited) {
      return (
        <span className="text-xs text-muted-foreground">
          Set on the project&apos;s card
        </span>
      )
    }
    return (
      <Button
        type="button"
        variant="ghost"
        size="sm"
        onClick={onRemove}
        disabled={removing}
        aria-label={`Remove internal access for ${name}`}
      >
        <Trash2 className="h-4 w-4" />
        <span className="max-sm:sr-only">
          {removing ? 'Removing…' : 'Remove'}
        </span>
      </Button>
    )
  }
  return (
    <>
      {limits}
      <Button
        type="button"
        size="sm"
        onClick={onAllow}
        disabled={paused !== false || allowing}
        aria-label={`Allow ${name}`}
      >
        {allowing
          ? 'Allowing…'
          : paused === 'pending'
            ? 'Checking…'
            : paused
              ? 'Paused'
              : 'Allow'}
      </Button>
    </>
  )
}

const ALL = 'all'

function environmentValue(id: number | null): string {
  return id === null ? ALL : String(id)
}

function parseEnvironmentValue(value: string): number | null {
  return value === ALL ? null : Number(value)
}

/**
 * The secondary path: allow with a narrower scope or a reason. The calling
 * project's environments and the target's load only once it opens.
 */
export function GrantLimitsPopover({
  name,
  callerProjectId,
  targetProjectId,
  targetName,
  fixedTargetEnvironmentId,
  disabled,
  pending,
  onSubmit,
}: {
  name: string
  /** Set for a project caller: its environments narrow "From". */
  callerProjectId?: number
  /** The project being called, for "May call". */
  targetProjectId: number
  targetName: string
  /** On an environment's card the target is fixed. */
  fixedTargetEnvironmentId?: number
  disabled: boolean
  pending: boolean
  onSubmit: (limits: GrantLimits) => Promise<unknown>
}) {
  const [open, setOpen] = useState(false)
  const form = useForm<GrantLimits>({
    resolver: zodResolver(grantLimitsSchema),
    defaultValues: defaultGrantLimits(fixedTargetEnvironmentId),
  })
  const callerEnvironments = useQuery({
    ...getEnvironmentsOptions({ path: { project_id: callerProjectId ?? 0 } }),
    enabled: open && callerProjectId !== undefined,
  })
  const targetEnvironments = useQuery({
    ...getEnvironmentsOptions({ path: { project_id: targetProjectId } }),
    enabled: open && fixedTargetEnvironmentId === undefined,
  })
  const submit = form.handleSubmit(async (limits) => {
    try {
      await onSubmit(limits)
      form.reset(defaultGrantLimits(fixedTargetEnvironmentId))
      setOpen(false)
    } catch {
      // The row's mutation already reported it.
    }
  })
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          disabled={disabled}
          aria-label={`Allow ${name} with limits`}
        >
          <SlidersHorizontal className="h-4 w-4" />
          <span className="max-sm:sr-only">Limit…</span>
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-80">
        <Form {...form}>
          <form
            onSubmit={submit}
            className="space-y-3"
            noValidate
            aria-label={`Allow ${name} with limits`}
          >
            {callerProjectId !== undefined ? (
              <FormField
                control={form.control}
                name="callerEnvironmentId"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>From</FormLabel>
                    <Select
                      value={environmentValue(field.value)}
                      onValueChange={(value) =>
                        field.onChange(parseEnvironmentValue(value))
                      }
                    >
                      <FormControl>
                        <SelectTrigger className="w-full">
                          <SelectValue />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        <SelectItem value={ALL}>
                          All its environments, including new ones
                        </SelectItem>
                        {callerEnvironments.data?.map((environment) => (
                          <SelectItem
                            key={environment.id}
                            value={String(environment.id)}
                          >
                            {environment.name} only
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                    <FormMessage />
                  </FormItem>
                )}
              />
            ) : null}
            {fixedTargetEnvironmentId === undefined ? (
              <FormField
                control={form.control}
                name="targetEnvironmentId"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>May call</FormLabel>
                    <Select
                      value={environmentValue(field.value)}
                      onValueChange={(value) =>
                        field.onChange(parseEnvironmentValue(value))
                      }
                    >
                      <FormControl>
                        <SelectTrigger className="w-full">
                          <SelectValue />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        <SelectItem value={ALL}>
                          Every environment of {targetName}
                        </SelectItem>
                        {targetEnvironments.data?.map((environment) => (
                          <SelectItem
                            key={environment.id}
                            value={String(environment.id)}
                          >
                            {environment.name} only
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                    <FormMessage />
                  </FormItem>
                )}
              />
            ) : null}
            <FormField
              control={form.control}
              name="description"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Reason (optional)</FormLabel>
                  <FormControl>
                    <Input
                      {...field}
                      maxLength={MAX_GRANT_DESCRIPTION_LENGTH}
                      placeholder="e.g. the agent tests the orders API"
                    />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />
            <Button type="submit" size="sm" disabled={pending}>
              {pending ? 'Allowing…' : 'Allow'}
            </Button>
          </form>
        </Form>
      </PopoverContent>
    </Popover>
  )
}
