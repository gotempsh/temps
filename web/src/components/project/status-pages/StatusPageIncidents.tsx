// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  createIncidentMutation,
  getIncidentUpdatesOptions,
  listIncidentsOptions,
  setIncidentUpdateVisibilityMutation,
  updateIncidentMutation,
  updateIncidentStatusMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { IncidentResponse, StatusPageResponse } from '@/api/client'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  Form,
  FormDescription,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { formatDistanceToNow } from 'date-fns'
import { zodResolver } from '@hookform/resolvers/zod'
import { Megaphone, Pencil, Plus } from 'lucide-react'
import { useState } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'
import {
  customIncidentBody,
  customIncidentSchema,
  incidentTargets,
  INCIDENT_SEVERITIES,
  INCIDENT_STATUSES,
  incidentUpdateSchema,
  initialIncidentStatus,
  MAX_INCIDENT_PUBLIC_TITLE_CHARS,
  operationQueries,
  type CustomIncidentValues,
  type IncidentUpdateValues,
} from './status-page-form'

const SEVERITY_HINTS: Record<(typeof INCIDENT_SEVERITIES)[number], string> = {
  minor: 'Degraded — slower or partly affected',
  major: 'Partial outage — some features down',
  critical: 'Major outage — the service is down',
}

/** Refresh everything an incident change can affect. */
async function invalidateIncidents(
  queryClient: ReturnType<typeof useQueryClient>
) {
  await queryClient.invalidateQueries(operationQueries('listIncidents'))
  await queryClient.invalidateQueries(operationQueries('getIncidentUpdates'))
  await queryClient.invalidateQueries(operationQueries('previewStatusPage'))
}

function reportDefaults(page: StatusPageResponse): CustomIncidentValues {
  return {
    public_title: '',
    severity: 'minor',
    status: 'investigating',
    target: incidentTargets(page)[0]?.value ?? '',
    message: '',
    is_public: true,
  }
}

function ReportIncidentDialog({
  page,
  open,
  onOpenChange,
}: {
  page: StatusPageResponse
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const queryClient = useQueryClient()
  const form = useForm<CustomIncidentValues>({
    resolver: zodResolver(customIncidentSchema),
    defaultValues: reportDefaults(page),
  })
  const targets = incidentTargets(page)
  const create = useMutation({
    ...createIncidentMutation(),
    meta: { errorTitle: 'Failed to report incident' },
    onSuccess: async (_incident, variables) => {
      await invalidateIncidents(queryClient)
      toast.success(
        variables.body.is_public
          ? 'Incident posted to published status pages.'
          : 'Incident recorded privately. Switch its update to public to show it.'
      )
      form.reset(reportDefaults(page))
      onOpenChange(false)
    },
  })

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>Report an incident</DialogTitle>
          <DialogDescription>
            Tell visitors about a problem your monitors cannot see — slow email,
            a third-party outage, planned maintenance — or add context to one
            they did.
          </DialogDescription>
        </DialogHeader>
        <Form {...form}>
          <form
            className="space-y-4"
            onSubmit={form.handleSubmit((values) => {
              const request = customIncidentBody(values, page)
              if (!request) {
                form.setError('target', {
                  message:
                    'That component is no longer on this page. Pick another.',
                })
                return
              }
              create.mutate({
                path: { project_id: request.project_id },
                body: request.body,
              })
            })}
          >
            <FormField
              control={form.control}
              name="public_title"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Headline</FormLabel>
                  <FormControl>
                    <Input
                      maxLength={MAX_INCIDENT_PUBLIC_TITLE_CHARS}
                      placeholder="Delayed email delivery"
                      {...field}
                    />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />
            <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
              <FormField
                control={form.control}
                name="target"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Affects</FormLabel>
                    <Select value={field.value} onValueChange={field.onChange}>
                      <FormControl>
                        <SelectTrigger className="w-full">
                          <SelectValue placeholder="Pick what is affected" />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        {targets.map((t) => (
                          <SelectItem
                            key={t.value}
                            value={t.value}
                            className={
                              t.value.startsWith('monitor:') &&
                              page.projects.length > 1
                                ? 'pl-6'
                                : undefined
                            }
                          >
                            {t.label}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                    <FormMessage />
                  </FormItem>
                )}
              />
              <FormField
                control={form.control}
                name="status"
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>Status</FormLabel>
                    <Select value={field.value} onValueChange={field.onChange}>
                      <FormControl>
                        <SelectTrigger className="w-full">
                          <SelectValue />
                        </SelectTrigger>
                      </FormControl>
                      <SelectContent>
                        {INCIDENT_STATUSES.map((s) => (
                          <SelectItem key={s} value={s}>
                            {titleCase(s)}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </FormItem>
                )}
              />
            </div>
            <FormField
              control={form.control}
              name="severity"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>Impact</FormLabel>
                  <div
                    role="radiogroup"
                    aria-label="Impact"
                    className="grid grid-cols-1 gap-2 sm:grid-cols-3"
                  >
                    {INCIDENT_SEVERITIES.map((severity) => (
                      <button
                        key={severity}
                        type="button"
                        role="radio"
                        aria-checked={field.value === severity}
                        onClick={() => field.onChange(severity)}
                        className={
                          'rounded-lg border p-2.5 text-left text-xs transition-colors hover:bg-muted/50 ' +
                          (field.value === severity
                            ? 'border-primary bg-primary/5'
                            : '')
                        }
                      >
                        <span className="block text-sm font-medium">
                          {titleCase(severity)}
                        </span>
                        <span className="text-muted-foreground">
                          {SEVERITY_HINTS[severity]}
                        </span>
                      </button>
                    ))}
                  </div>
                </FormItem>
              )}
            />
            <FormField
              control={form.control}
              name="message"
              render={({ field }) => (
                <FormItem>
                  <FormLabel>First update</FormLabel>
                  <FormControl>
                    <Textarea
                      rows={3}
                      placeholder="Some emails are arriving up to 10 minutes late. We are working with our provider."
                      {...field}
                    />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />
            <FormField
              control={form.control}
              name="is_public"
              render={({ field }) => (
                <FormItem className="flex flex-row items-center justify-between gap-4 space-y-0 rounded-lg border p-3">
                  <div>
                    <FormLabel>Post to status pages now</FormLabel>
                    <FormDescription>
                      Off records it privately; you can publish updates later.
                    </FormDescription>
                  </div>
                  <FormControl>
                    <Switch
                      checked={field.value}
                      onCheckedChange={field.onChange}
                    />
                  </FormControl>
                </FormItem>
              )}
            />
            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                onClick={() => onOpenChange(false)}
              >
                Cancel
              </Button>
              <Button type="submit" disabled={create.isPending}>
                {create.isPending ? 'Posting...' : 'Report incident'}
              </Button>
            </DialogFooter>
          </form>
        </Form>
      </DialogContent>
    </Dialog>
  )
}

/** The incident's public headline, editable inline. */
function PublicTitleEditor({ incident }: { incident: IncidentResponse }) {
  const queryClient = useQueryClient()
  const [editing, setEditing] = useState(false)
  const [value, setValue] = useState(incident.public_title ?? '')
  const save = useMutation({
    ...updateIncidentMutation(),
    meta: { errorTitle: 'Failed to change the public headline' },
    onSuccess: async (saved) => {
      await invalidateIncidents(queryClient)
      setEditing(false)
      toast.success(
        saved.public_title
          ? 'Public headline saved.'
          : 'Public headline cleared; pages show one based on impact.'
      )
    },
  })
  if (!editing) {
    return (
      <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
        <span>
          Visitors see:{' '}
          <span className="font-medium text-foreground">
            {incident.public_title || severityHeading(incident.severity)}
          </span>
          {!incident.public_title && ' (based on impact)'}
        </span>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="h-6 px-2"
          onClick={() => {
            setValue(incident.public_title ?? '')
            setEditing(true)
          }}
        >
          <Pencil className="mr-1 size-3" />
          Edit headline
        </Button>
      </div>
    )
  }
  return (
    <form
      className="flex flex-col gap-2 sm:flex-row"
      onSubmit={(e) => {
        e.preventDefault()
        save.mutate({
          path: { incident_id: incident.id },
          body: { public_title: value },
        })
      }}
    >
      <Input
        autoFocus
        value={value}
        maxLength={MAX_INCIDENT_PUBLIC_TITLE_CHARS}
        placeholder={severityHeading(incident.severity)}
        aria-label="Public headline"
        onChange={(e) => setValue(e.target.value)}
      />
      <div className="flex gap-2">
        <Button type="submit" size="sm" disabled={save.isPending}>
          {save.isPending ? 'Saving...' : 'Save'}
        </Button>
        <Button
          type="button"
          size="sm"
          variant="outline"
          onClick={() => setEditing(false)}
        >
          Cancel
        </Button>
      </div>
    </form>
  )
}

/** Mirrors the server's fallback heading for incidents without one. */
function severityHeading(severity: string) {
  switch (severity) {
    case 'critical':
      return 'Major outage'
    case 'major':
      return 'Partial outage'
    default:
      return 'Degraded service'
  }
}

/** Incidents per page of the incident list. */
const INCIDENTS_PAGE_SIZE = 5

function titleCase(value: string) {
  return value.charAt(0).toUpperCase() + value.slice(1)
}

function PostUpdateForm({ incident }: { incident: IncidentResponse }) {
  const queryClient = useQueryClient()
  const form = useForm<IncidentUpdateValues>({
    resolver: zodResolver(incidentUpdateSchema),
    defaultValues: {
      status: initialIncidentStatus(incident.status),
      message: '',
      is_public: true,
    },
  })
  const post = useMutation({
    ...updateIncidentStatusMutation(),
    meta: { errorTitle: 'Failed to post incident update' },
    onSuccess: async (_incident, variables) => {
      await invalidateIncidents(queryClient)
      form.reset({ ...form.getValues(), message: '' })
      toast.success(
        variables.body.is_public
          ? 'Update posted and shown on published status pages.'
          : 'Private update posted.'
      )
    },
  })
  return (
    <Form {...form}>
      <form
        className="space-y-2"
        onSubmit={form.handleSubmit((values) =>
          post.mutate({ path: { incident_id: incident.id }, body: values })
        )}
      >
        <FormField
          control={form.control}
          name="message"
          render={({ field }) => (
            <FormItem>
              <FormControl>
                <Textarea
                  rows={2}
                  placeholder="We have identified the cause and are rolling out a fix."
                  aria-label="Update message"
                  {...field}
                />
              </FormControl>
              <FormMessage />
            </FormItem>
          )}
        />
        <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
          <FormField
            control={form.control}
            name="status"
            render={({ field }) => (
              <FormItem>
                <Select value={field.value} onValueChange={field.onChange}>
                  <FormControl>
                    <SelectTrigger
                      className="w-full sm:w-[180px]"
                      aria-label="Status"
                    >
                      <SelectValue />
                    </SelectTrigger>
                  </FormControl>
                  <SelectContent>
                    {INCIDENT_STATUSES.map((s) => (
                      <SelectItem key={s} value={s}>
                        {titleCase(s)}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </FormItem>
            )}
          />
          <FormField
            control={form.control}
            name="is_public"
            render={({ field }) => (
              <FormItem className="flex flex-row items-center gap-2 space-y-0">
                <FormControl>
                  <Checkbox
                    checked={field.value}
                    onCheckedChange={(checked) =>
                      field.onChange(checked === true)
                    }
                  />
                </FormControl>
                <FormLabel>Show on status pages</FormLabel>
              </FormItem>
            )}
          />
          <Button
            type="submit"
            size="sm"
            className="sm:ml-auto"
            disabled={post.isPending}
          >
            {post.isPending ? 'Posting...' : 'Post update'}
          </Button>
        </div>
      </form>
    </Form>
  )
}

function IncidentRow({ incident }: { incident: IncidentResponse }) {
  const queryClient = useQueryClient()
  const updates = useQuery(
    getIncidentUpdatesOptions({ path: { incident_id: incident.id } })
  )
  const toggle = useMutation({
    ...setIncidentUpdateVisibilityMutation(),
    meta: { errorTitle: 'Failed to change update visibility' },
    onSuccess: async (update) => {
      await invalidateIncidents(queryClient)
      toast.success(
        update.is_public
          ? 'Update is now shown on published status pages.'
          : 'Update is now private.'
      )
    },
  })

  return (
    <div className="space-y-3 p-4">
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-medium">{incident.title}</span>
        <Badge variant="neutral-subtle">{titleCase(incident.severity)}</Badge>
        <Badge
          variant={
            incident.status === 'resolved' ? 'success-subtle' : 'warning-subtle'
          }
        >
          {titleCase(incident.status)}
        </Badge>
        <span className="text-xs text-muted-foreground">
          started{' '}
          {formatDistanceToNow(new Date(incident.started_at), {
            addSuffix: true,
          })}
        </span>
      </div>
      <PublicTitleEditor
        key={incident.public_title ?? ''}
        incident={incident}
      />
      {updates.isLoading ? (
        <Skeleton className="h-10 w-full" />
      ) : updates.isError ? (
        <p className="text-sm text-destructive">Failed to load updates.</p>
      ) : (
        <ul className="space-y-2">
          {(updates.data ?? []).map((update) => (
            <li
              key={update.id}
              className="flex flex-col gap-2 border-l-2 pl-3 sm:flex-row sm:items-start sm:justify-between"
            >
              <div className="min-w-0">
                <div className="text-xs text-muted-foreground">
                  {titleCase(update.status)} ·{' '}
                  {formatDistanceToNow(new Date(update.created_at), {
                    addSuffix: true,
                  })}
                </div>
                <p className="whitespace-pre-wrap break-words text-sm">
                  {update.message}
                </p>
              </div>
              <div className="flex shrink-0 items-center gap-2">
                <Switch
                  checked={update.is_public}
                  disabled={toggle.isPending || update.is_system}
                  aria-label="Show this update on status pages"
                  onCheckedChange={(checked) =>
                    toggle.mutate({
                      path: {
                        incident_id: incident.id,
                        update_id: update.id,
                      },
                      body: { is_public: checked },
                    })
                  }
                />
                <span className="text-xs text-muted-foreground">
                  {update.is_system
                    ? 'Private (automatic)'
                    : update.is_public
                      ? 'Public'
                      : 'Private'}
                </span>
              </div>
            </li>
          ))}
        </ul>
      )}
      <PostUpdateForm incident={incident} />
    </div>
  )
}

/**
 * Recent incidents of the projects a page shows, one project at a time, with
 * the controls that decide what of them reaches published status pages.
 * Incident updates are private by default.
 */
export function StatusPageIncidents({
  page: statusPage,
}: {
  page: StatusPageResponse
}) {
  const [page, setPage] = useState(1)
  const [reportOpen, setReportOpen] = useState(false)
  const [selectedProjectId, setSelectedProjectId] = useState<number>()
  // The selection can leave the page when its components are edited.
  const project =
    statusPage.projects.find((p) => p.id === selectedProjectId) ??
    statusPage.projects[0]
  const projectId = project?.id
  const incidents = useQuery({
    ...listIncidentsOptions({
      path: { project_id: projectId ?? 0 },
      query: { page, page_size: INCIDENTS_PAGE_SIZE },
    }),
    enabled: projectId !== undefined,
    // Keep the previous page visible while the next one loads, but only
    // for this project.
    placeholderData: (previous, previousQuery) =>
      previousQuery?.queryKey[0]?.path?.project_id === projectId
        ? previous
        : undefined,
  })
  const items = incidents.data?.incidents ?? []
  const total = incidents.data?.total ?? 0
  const totalPages = Math.max(1, Math.ceil(total / INCIDENTS_PAGE_SIZE))

  return (
    <section className="space-y-3">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <h3 className="text-lg font-semibold">Incidents</h3>
          <p className="max-w-2xl text-sm text-muted-foreground">
            Incidents your monitors open are private until you post a public
            update. Report your own to tell visitors about anything else. Public
            updates appear on published pages when the incident concerns a
            component shown there, or a whole project the page shows.
          </p>
        </div>
        <div className="flex shrink-0 flex-col gap-2 self-start sm:flex-row">
          {statusPage.projects.length > 1 && project && (
            <Select
              value={String(project.id)}
              onValueChange={(value) => {
                setSelectedProjectId(Number(value))
                setPage(1)
              }}
            >
              <SelectTrigger
                className="w-full sm:w-[200px]"
                aria-label="Show incidents of project"
              >
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {statusPage.projects.map((p) => (
                  <SelectItem key={p.id} value={String(p.id)}>
                    {p.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
          <Button
            type="button"
            disabled={!project}
            onClick={() => setReportOpen(true)}
          >
            <Plus className="size-4 sm:mr-2" />
            <span className="max-sm:sr-only">Report incident</span>
          </Button>
        </div>
      </div>
      {!project ? (
        <p className="rounded-lg border border-dashed p-4 text-sm text-muted-foreground">
          This page has no components, so it has no incidents. Add a component
          in the Components tab first.
        </p>
      ) : incidents.isError ? (
        <ErrorAlert
          title="Failed to load incidents"
          description={
            incidents.error instanceof Error
              ? incidents.error.message
              : 'An unexpected error occurred'
          }
          retry={() => incidents.refetch()}
        />
      ) : incidents.isLoading ? (
        <Skeleton className="h-24 w-full" />
      ) : items.length === 0 ? (
        <div className="flex flex-col items-start gap-3 rounded-lg border border-dashed p-4 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex items-start gap-3">
            <Megaphone className="mt-0.5 size-5 shrink-0 text-muted-foreground" />
            <p className="text-sm text-muted-foreground">
              No incidents. When a monitor detects an outage it appears here so
              you can post public updates — or report one yourself, e.g.
              “Delayed email delivery” or “Scheduled database maintenance”.
            </p>
          </div>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => setReportOpen(true)}
          >
            Report incident
          </Button>
        </div>
      ) : (
        <Card>
          <CardContent className="divide-y p-0">
            {items.map((incident) => (
              <IncidentRow key={incident.id} incident={incident} />
            ))}
          </CardContent>
        </Card>
      )}
      {/* Also shown past the last page (incidents can disappear while
          browsing), so there is always a way back. */}
      {(totalPages > 1 || page > 1) && (
        <ResponsivePagination
          page={page}
          pageSize={INCIDENTS_PAGE_SIZE}
          total={total}
          totalPages={Math.max(totalPages, page)}
          onPageChange={setPage}
          ariaLabel="Incident pagination"
        />
      )}
      <ReportIncidentDialog
        page={statusPage}
        open={reportOpen}
        onOpenChange={setReportOpen}
      />
    </section>
  )
}
