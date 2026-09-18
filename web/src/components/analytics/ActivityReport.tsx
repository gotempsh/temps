// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from 'react'
import { useForm, useFieldArray, useWatch, Controller } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'
import { Link } from 'react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { AlertCircle, Loader2, Sparkles } from 'lucide-react'
import { toast } from 'sonner'
import {
  getActivityStatusOptions,
  getEnvironmentsOptions,
  getEnvironmentDomainsOptions,
  suggestActivityGoalsMutation,
  previewActivityReportMutation,
  runActivityReportMutation,
  saveActivitySettingsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  ActivityGoal,
  ActivityPreview,
  ActivityStatus,
  ProjectResponse,
  EnvironmentResponse,
} from '@/api/client/types.gen'
import { PageHeader } from '@/components/layout/PageContainer'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Textarea } from '@/components/ui/textarea'
import { Switch } from '@/components/ui/switch'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'

function errorMessage(error: unknown): string {
  if (
    error &&
    typeof error === 'object' &&
    'detail' in error &&
    typeof error.detail === 'string'
  )
    return error.detail
  return 'Could not complete the request. Check the AI provider settings and try again.'
}

export function ActivityReportPage({ project }: { project: ProjectResponse }) {
  const queryClient = useQueryClient()
  const [environmentId, setEnvironmentId] = useState<number | null>(null)
  const environments = useQuery(
    getEnvironmentsOptions({ path: { project_id: project.id } })
  )
  const options = getActivityStatusOptions({
    path: { project_id: project.id },
    query:
      environmentId == null ? undefined : { environment_id: environmentId },
  })
  const status = useQuery({
    ...options,
    placeholderData: (previous) => previous,
    refetchInterval: (query) => (query.state.data?.running ? 3000 : 30_000),
  })
  const [preview, setPreview] = useState<ActivityPreview | null>(null)
  const [category, setCategory] = useState<string | null>(null)
  const run = useMutation({
    ...runActivityReportMutation(),
    onSuccess: () => {
      setCategory(null)
      setPreview(null)
      toast.success('Activity report ready')
    },
    onError: (error) => toast.error(errorMessage(error)),
    onSettled: () =>
      queryClient.invalidateQueries({ queryKey: options.queryKey }),
  })
  const data = status.data
  const report = preview?.report ?? data?.report
  const running = run.isPending || data?.running
  const visitors =
    report?.visitors.filter(
      (visitor) => !category || visitor.categories.includes(category)
    ) ?? []
  const categories = [
    ...new Set(report?.visitors.flatMap((visitor) => visitor.categories) ?? []),
  ]

  return (
    <div className="space-y-6">
      <PageHeader
        title="Activity report"
        description="Understand what visitors are doing in the context of your application."
      />
      {status.isPending && (
        <div
          role="status"
          aria-label="Loading activity report"
          className="space-y-4"
        >
          <Skeleton className="h-48 w-full" />
          <Skeleton className="h-64 w-full" />
        </div>
      )}
      {status.isError && (
        <Alert variant="destructive">
          <AlertDescription>
            Could not load the activity report.{' '}
            <Button variant="link" onClick={() => status.refetch()}>
              Retry
            </Button>
          </AlertDescription>
        </Alert>
      )}
      {data && (
        <>
          {!data.configured && (
            <Alert>
              <Sparkles className="h-4 w-4" />
              <AlertDescription>
                No AI Gateway provider is ready. Connect one to turn page visits
                and custom events into a daily explanation of visitor activity.
                For example: “Readers explored migration guides, then checked
                pricing.”{' '}
                <Link className="underline" to={data.setup_url}>
                  Configure AI provider
                </Link>
              </AlertDescription>
            </Alert>
          )}
          {!status.isFetching && !data.has_recent_activity && (
            <Alert>
              <AlertDescription>
                <strong>
                  No tracked visitor activity in this environment in the last 24
                  hours.
                </strong>{' '}
                There is nothing to analyze yet. You can discover goals and edit
                your setup now. Preview becomes available after a visitor
                records a page view or custom event. Bots and visitors without
                tracked events are excluded.
              </AlertDescription>
            </Alert>
          )}
          <ActivitySettingsForm
            key={data.settings_revision}
            projectId={project.id}
            status={data}
            environments={environments.data ?? []}
            environmentLoading={environments.isPending}
            environmentError={environments.isError}
            activityLoading={status.isFetching || status.isError}
            onEnvironmentChange={setEnvironmentId}
            disabled={!!running}
            onPreview={(value) => {
              setPreview(value)
              setCategory(null)
            }}
            onSaved={() =>
              queryClient.invalidateQueries({ queryKey: options.queryKey })
            }
          />
          <Card>
            <CardHeader>
              <div className="flex flex-wrap items-start justify-between gap-3">
                <div>
                  <CardTitle>
                    {preview
                      ? 'Preview of visitor activity'
                      : 'Recent visitor activity'}
                  </CardTitle>
                  <CardDescription>
                    Analyzes the previous 24 hours in the selected environment.
                    Anonymous visitors are included.
                    {report?.environment_id &&
                      ` Report environment: ${environments.data?.find((env) => env.id === report.environment_id)?.name ?? 'Unavailable environment'}.`}
                  </CardDescription>
                </div>
                {data.has_recent_activity && (
                  <Button
                    disabled={
                      !data.configured ||
                      !data.settings_revision ||
                      !data.settings.share_activity_with_ai ||
                      !!running ||
                      status.isFetching ||
                      status.isError ||
                      data.selected_environment_id !==
                        data.settings.environment_id
                    }
                    onClick={() =>
                      run.mutate({ path: { project_id: project.id } })
                    }
                  >
                    {running ? (
                      <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                    ) : (
                      <Sparkles className="mr-2 h-4 w-4" />
                    )}
                    {running ? 'Analyzing…' : 'Run saved settings'}
                  </Button>
                )}
              </div>
            </CardHeader>
            <CardContent className="space-y-5">
              <p className="text-sm text-muted-foreground">
                A sample of up to 20 recently active visitors from the last 24
                hours. Each category links back to observed activity.
              </p>
              {data.selected_environment_id !==
                data.settings.environment_id && (
                <p className="text-sm text-muted-foreground">
                  Save your environment selection before running the saved
                  report.
                </p>
              )}
              {data.next_run_at && (
                <p className="text-sm">
                  Next daily run: {new Date(data.next_run_at).toLocaleString()}{' '}
                  (within five minutes).
                </p>
              )}
              {data.last_error && (
                <Alert variant="destructive">
                  <AlertCircle className="h-4 w-4" />
                  <AlertDescription>{data.last_error}</AlertDescription>
                </Alert>
              )}
              {!report ? (
                <div className="rounded-lg border border-dashed p-6 text-sm text-muted-foreground">
                  {data.has_recent_activity
                    ? 'Describe what you want to understand above, then preview your visitors. Temps will suggest categories and show the activity behind them.'
                    : 'Waiting for visitor activity. This page checks for new activity automatically.'}
                </div>
              ) : (
                <>
                  <div className="space-y-2">
                    <p className="whitespace-pre-wrap">{report.summary}</p>
                    <p className="text-xs text-muted-foreground">
                      {new Date(report.window_start).toLocaleString()} –{' '}
                      {new Date(report.window_end).toLocaleString()} ·{' '}
                      {report.visitors.length} visitors assessed ·{' '}
                      {report.events_considered} events considered
                      {report.model ? ` · ${report.model}` : ''}
                    </p>
                    <p className="text-xs text-muted-foreground">
                      AI interpretation of observed activity, not confirmed
                      intent. Category counts can overlap.
                    </p>
                    {report.sampled && (
                      <p className="text-sm text-amber-700 dark:text-amber-400">
                        Sampled report: some visitors or events were excluded by
                        the run limits. These counts do not represent all
                        traffic.
                      </p>
                    )}
                    {preview && (
                      <p className="text-sm text-muted-foreground">
                        Preview only. This sample is not stored as your daily
                        report.
                      </p>
                    )}
                    {!preview &&
                      report.settings_revision !== data.settings_revision && (
                        <p className="text-sm text-muted-foreground">
                          Settings have changed since this report. Run again to
                          apply them.
                        </p>
                      )}
                  </div>
                  <div
                    className="flex flex-wrap gap-2"
                    aria-label="Filter assessments by category"
                  >
                    <Button
                      size="sm"
                      variant={category === null ? 'default' : 'outline'}
                      onClick={() => setCategory(null)}
                    >
                      All ({report.visitors.length})
                    </Button>
                    {categories.map((name) => (
                      <Button
                        key={name}
                        size="sm"
                        variant={category === name ? 'default' : 'outline'}
                        onClick={() => setCategory(name)}
                      >
                        {name} (
                        {
                          report.visitors.filter((visitor) =>
                            visitor.categories.includes(name)
                          ).length
                        }
                        )
                      </Button>
                    ))}
                  </div>
                  <div className="divide-y">
                    {visitors.map((visitor) => (
                      <article
                        key={visitor.visitor_id}
                        className="space-y-3 py-4"
                      >
                        <div className="flex flex-wrap items-center gap-2">
                          <Link
                            className="font-medium underline underline-offset-4"
                            to={`/projects/${project.slug}/analytics/visitors/${visitor.visitor_id}`}
                          >
                            Visitor #{visitor.visitor_id}
                          </Link>
                          {visitor.categories.map((name) => (
                            <Badge key={name} variant="secondary">
                              {name}
                            </Badge>
                          ))}
                        </div>
                        <p className="text-sm">{visitor.explanation}</p>
                        <details className="text-sm">
                          <summary className="cursor-pointer text-muted-foreground">
                            Supporting activity ({visitor.evidence.length})
                          </summary>
                          <ol className="mt-2 space-y-2 border-l pl-4">
                            {visitor.evidence.map((event) => (
                              <li key={event.reference} className="break-words">
                                <span className="font-medium">
                                  {event.event}
                                </span>{' '}
                                · {event.path}
                                {event.title && ` — ${event.title}`}
                                <br />
                                <span className="text-xs text-muted-foreground">
                                  {new Date(event.timestamp).toLocaleString()}
                                  {event.properties
                                    .map(
                                      (property) =>
                                        ` · ${property.key}: ${property.value}`
                                    )
                                    .join('')}
                                </span>
                              </li>
                            ))}
                          </ol>
                        </details>
                      </article>
                    ))}
                  </div>
                </>
              )}
            </CardContent>
          </Card>
        </>
      )}
    </div>
  )
}

const setupSchema = z.object({
  environment_id: z.number().int().positive().nullable(),
  source_url: z.string().nullable(),
  source_domain: z.string().nullable(),
  application_context: z
    .string()
    .trim()
    .min(1, 'Describe your application and what you want to understand.')
    .max(4000),
  categories: z.array(z.object({ name: z.string(), description: z.string() })),
  propertyKeys: z.string(),
  share_activity_with_ai: z.boolean(),
  daily_enabled: z.boolean(),
})
type SetupForm = z.infer<typeof setupSchema>

function ActivitySettingsForm({
  projectId,
  status,
  environments,
  environmentLoading,
  environmentError,
  activityLoading,
  onEnvironmentChange,
  disabled,
  onSaved,
  onPreview,
}: {
  projectId: number
  status: ActivityStatus
  environments: EnvironmentResponse[]
  environmentLoading: boolean
  environmentError: boolean
  activityLoading: boolean
  onEnvironmentChange: (id: number) => void
  disabled: boolean
  onSaved: () => Promise<unknown>
  onPreview: (preview: ActivityPreview | null) => void
}) {
  const form = useForm<SetupForm>({
    resolver: zodResolver(setupSchema),
    defaultValues: {
      ...status.settings,
      environment_id: status.selected_environment_id ?? null,
      source_url: status.settings.source_url ?? null,
      source_domain: status.settings.source_domain ?? null,
      propertyKeys: status.settings.property_keys.join(', '),
    },
  })
  const [selectedEnvironment, sourceUrl, sourceDomain] = useWatch({
    control: form.control,
    name: ['environment_id', 'source_url', 'source_domain'],
  })
  const environment = environments.find(
    (item) => item.id === selectedEnvironment
  )
  const canPreview =
    !activityLoading &&
    selectedEnvironment != null &&
    status.selected_environment_id === selectedEnvironment &&
    status.has_recent_activity
  const { fields, append, remove } = useFieldArray({
    control: form.control,
    name: 'categories',
  })
  const [categories, shareActivity, dailyEnabled] = useWatch({
    control: form.control,
    name: ['categories', 'share_activity_with_ai', 'daily_enabled'],
  })
  const values = {
    categories,
    share_activity_with_ai: shareActivity,
    daily_enabled: dailyEnabled,
  }
  const [hasSetup, setHasSetup] = useState(status.settings_revision > 0)
  const preview = useMutation({
    ...previewActivityReportMutation(),
    onSuccess: (result) => {
      form.reset({
        ...result.settings,
        // A preview suggests classification settings, not a scheduling change.
        daily_enabled: form.getValues('daily_enabled'),
        environment_id: form.getValues('environment_id'),
        source_url: form.getValues('source_url'),
        source_domain: form.getValues('source_domain'),
        propertyKeys: result.settings.property_keys.join(', '),
      })
      setHasSetup(true)
      onPreview(result)
      toast.success('Your visitor preview is ready')
    },
    onError: (error) => toast.error(errorMessage(error)),
  })
  const save = useMutation({
    ...saveActivitySettingsMutation(),
    onSuccess: async () => {
      toast.success('Activity report setup saved')
      await onSaved()
    },
    onError: (error) => toast.error(errorMessage(error)),
  })
  const properties = (value: string) =>
    value
      .split(',')
      .map((key) => key.trim())
      .filter(Boolean)
  const saveSetup = (daily: boolean) =>
    form.handleSubmit((data) => {
      save.mutate({
        path: { project_id: projectId },
        body: {
          environment_id: data.environment_id,
          source_url: data.source_url,
          source_domain: data.source_domain,
          application_context: data.application_context,
          categories: data.categories,
          property_keys: properties(data.propertyKeys),
          share_activity_with_ai: data.share_activity_with_ai,
          daily_enabled: daily && data.share_activity_with_ai,
        },
      })
    })()
  const busy = disabled || save.isPending || preview.isPending

  return (
    <Card>
      <CardHeader>
        <CardTitle>
          What would you like to understand about your visitors?
        </CardTitle>
        <CardDescription>
          Start with suggestions from your deployed app, or describe it
          yourself. Preview real visitor activity before enabling reports.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-6">
        <div className="space-y-2">
          <Label htmlFor="activity-environment">Analyze activity from</Label>
          {environmentLoading ? (
            <Skeleton className="h-10 w-full" />
          ) : (
            <Select
              value={selectedEnvironment?.toString() ?? ''}
              disabled={busy || environments.length === 0}
              onValueChange={(value) => {
                const id = Number(value)
                form.setValue('environment_id', id, { shouldDirty: true })
                form.setValue('source_domain', null, { shouldDirty: true })
                onPreview(null)
                preview.reset()
                onEnvironmentChange(id)
              }}
            >
              <SelectTrigger id="activity-environment">
                <SelectValue placeholder="Choose an environment" />
              </SelectTrigger>
              <SelectContent>
                {environments.map((item) => (
                  <SelectItem key={item.id} value={String(item.id)}>
                    {item.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
          <p className="text-sm text-muted-foreground">
            Only this environment’s visitors and events are analyzed. Daily
            reports use your saved selection.
          </p>
          {environmentError && (
            <p role="alert" className="text-sm text-destructive">
              Could not load environments. Refresh the page to try again.
            </p>
          )}
          {!environmentLoading &&
            !environmentError &&
            environments.length === 0 && (
              <p className="text-sm text-muted-foreground">
                Create an environment before setting up an activity report.
              </p>
            )}
          {activityLoading && (
            <p role="status" className="text-sm text-muted-foreground">
              Checking recent activity…
            </p>
          )}
        </div>
        <GoalSuggestions
          key={selectedEnvironment ?? 'none'}
          projectId={projectId}
          environment={environment}
          sourceUrl={sourceUrl}
          sourceDomain={sourceDomain}
          onSourceChange={(url, domain) => {
            form.setValue('source_url', url, { shouldDirty: true })
            form.setValue('source_domain', domain, { shouldDirty: true })
          }}
          configured={status.configured}
          disabled={busy}
          onSelect={(goal) => {
            form.setValue('application_context', goal.goal, {
              shouldDirty: true,
            })
            setHasSetup(false)
            onPreview(null)
            preview.reset()
            form.setFocus('application_context')
          }}
        />
        <form
          className="space-y-5"
          onSubmit={form.handleSubmit((data) => {
            if (!canPreview) return
            onPreview(null)
            preview.mutate({
              path: { project_id: projectId },
              body: {
                environment_id: data.environment_id,
                source_url: data.source_url,
                source_domain: data.source_domain,
                goal: data.application_context,
                property_keys: properties(data.propertyKeys),
                share_activity_with_ai: data.share_activity_with_ai,
              },
            })
          })}
          onChange={() => {
            onPreview(null)
            preview.reset()
          }}
        >
          <fieldset disabled={busy} className="space-y-5 disabled:opacity-60">
            <div className="space-y-2">
              <Label htmlFor="activity-context">
                Your application and goal
              </Label>
              <Textarea
                id="activity-context"
                rows={4}
                maxLength={4000}
                {...form.register('application_context', {
                  onChange: () => setHasSetup(false),
                })}
                placeholder="We sell a self-hosted deployment platform. Help me understand who’s reading to learn, who’s considering migrating, and who’s struggling to get started."
                aria-invalid={!!form.formState.errors.application_context}
                aria-describedby="activity-goal-help"
              />
              <p
                id="activity-goal-help"
                className="text-sm text-muted-foreground"
              >
                {form.formState.errors.application_context?.message ??
                  'You can refine this description and preview again before saving.'}
              </p>
            </div>
            {hasSetup && (
              <div className="space-y-2">
                <p className="text-sm font-medium">Categories for this setup</p>
                <div className="flex flex-wrap gap-2">
                  {values.categories.map((category, index) => (
                    <Badge
                      key={index}
                      variant="secondary"
                      title={category.description}
                    >
                      {category.name}
                    </Badge>
                  ))}
                  <Badge variant="outline">Insufficient evidence</Badge>
                </div>
                <p className="text-xs text-muted-foreground">
                  Categories can overlap. Reading a page alone does not confirm
                  intent.
                </p>
              </div>
            )}
            <details>
              <summary className="cursor-pointer text-sm font-medium">
                Advanced settings
              </summary>
              <div className="mt-4 space-y-4">
                <div className="space-y-2">
                  <Label htmlFor="activity-properties">
                    Custom event property keys (optional)
                  </Label>
                  <Input
                    id="activity-properties"
                    {...form.register('propertyKeys')}
                    placeholder="plan, content_topic, setup_step"
                  />
                  <p className="text-xs text-muted-foreground">
                    Comma-separated, up to 10. Only these property values are
                    shared. Avoid personal information and secrets.
                  </p>
                </div>
                {hasSetup && (
                  <div className="space-y-3">
                    <p className="text-sm text-muted-foreground">
                      Edit these categories and save to use your changes.
                      Previewing again generates new categories from your
                      description.
                    </p>
                    {fields.map((field, index) => (
                      <div
                        key={field.id}
                        className="grid items-start gap-2 sm:grid-cols-[1fr_2fr_auto]"
                      >
                        <Input
                          aria-label={`Category ${index + 1} name`}
                          maxLength={60}
                          {...form.register(`categories.${index}.name`)}
                        />
                        <Textarea
                          aria-label={`Category ${index + 1} definition`}
                          maxLength={500}
                          rows={2}
                          {...form.register(`categories.${index}.description`)}
                        />
                        <Button
                          type="button"
                          variant="ghost"
                          disabled={fields.length === 1}
                          aria-label={`Remove category ${index + 1}`}
                          onClick={() => {
                            remove(index)
                            onPreview(null)
                          }}
                        >
                          Remove
                        </Button>
                      </div>
                    ))}
                    <Button
                      type="button"
                      size="sm"
                      variant="outline"
                      disabled={fields.length >= 8}
                      onClick={() => {
                        append({ name: '', description: '' })
                        onPreview(null)
                      }}
                    >
                      Add category
                    </Button>
                  </div>
                )}
                {status.settings_revision > 0 && (
                  <label className="flex items-center gap-3 text-sm">
                    <Controller
                      control={form.control}
                      name="daily_enabled"
                      render={({ field }) => (
                        <Switch
                          checked={field.value}
                          onCheckedChange={field.onChange}
                          onBlur={field.onBlur}
                          ref={field.ref}
                          disabled={!values.share_activity_with_ai}
                        />
                      )}
                    />
                    Run automatically every 24 hours
                  </label>
                )}
              </div>
            </details>
            <div className="space-y-2">
              <label className="flex items-start gap-3 text-sm">
                <Controller
                  control={form.control}
                  name="share_activity_with_ai"
                  render={({ field }) => (
                    <Switch
                      className="mt-0.5"
                      checked={field.value}
                      onBlur={field.onBlur}
                      ref={field.ref}
                      onCheckedChange={(checked) => {
                        field.onChange(checked)
                        if (!checked) form.setValue('daily_enabled', false)
                        onPreview(null)
                        preview.reset()
                      }}
                    />
                  )}
                />
                <span>Allow analysis using my configured AI provider.</span>
              </label>
              <p className="text-xs text-muted-foreground">
                Your description, categories, page paths, titles, event names,
                timestamps, and selected properties will be sent to it. These
                fields may contain personal information. Database visitor IDs,
                IP addresses, and URL query strings are excluded.
              </p>
            </div>
            {preview.isError && (
              <Alert variant="destructive">
                <AlertDescription>
                  {errorMessage(preview.error)} Your saved setup has not
                  changed.
                </AlertDescription>
              </Alert>
            )}
            <div className="flex flex-wrap gap-2">
              {canPreview && (
                <Button
                  type="submit"
                  disabled={
                    !status.configured || !values.share_activity_with_ai
                  }
                >
                  {preview.isPending ? (
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  ) : (
                    <Sparkles className="mr-2 h-4 w-4" />
                  )}
                  {preview.isPending
                    ? 'Preparing preview…'
                    : 'Preview my visitors'}
                </Button>
              )}
              {hasSetup && (
                <Button
                  type="button"
                  variant="outline"
                  disabled={
                    !status.configured ||
                    !values.share_activity_with_ai ||
                    selectedEnvironment == null
                  }
                  onClick={() => saveSetup(true)}
                >
                  Enable daily reports
                </Button>
              )}
              <Button
                type="button"
                variant="ghost"
                disabled={selectedEnvironment == null}
                onClick={() =>
                  saveSetup(
                    status.settings_revision > 0 && values.daily_enabled
                  )
                }
              >
                {save.isPending
                  ? 'Saving…'
                  : status.settings_revision > 0
                    ? 'Save settings'
                    : hasSetup
                      ? 'Save for manual reports'
                      : 'Save setup'}
              </Button>
            </div>
            {preview.isPending && (
              <p role="status" className="text-sm text-muted-foreground">
                Suggesting categories and checking recent activity. Your saved
                setup and schedule stay unchanged.
              </p>
            )}
            {hasSetup && (
              <p className="text-xs text-muted-foreground">
                Saved categories are reused for future reports. Daily reporting
                starts only when you enable it.
              </p>
            )}
          </fieldset>
        </form>
      </CardContent>
    </Card>
  )
}

const discoverySchema = z.object({
  url: z.string().url('Enter the public URL of your application.').max(2048),
  share: z
    .boolean()
    .refine(
      (value) => value,
      'Allow sharing public content to generate suggestions.'
    ),
})
type DiscoveryForm = z.infer<typeof discoverySchema>

function GoalSuggestions({
  projectId,
  environment,
  sourceUrl,
  sourceDomain,
  onSourceChange,
  configured,
  disabled,
  onSelect,
}: {
  projectId: number
  environment?: EnvironmentResponse
  sourceUrl: string | null
  sourceDomain: string | null
  onSourceChange: (url: string | null, domain: string | null) => void
  configured: boolean
  disabled: boolean
  onSelect: (goal: ActivityGoal) => void
}) {
  const domains = useQuery({
    ...getEnvironmentDomainsOptions({
      path: { project_id: projectId, env_id: environment?.id ?? 0 },
    }),
    enabled: environment != null,
  })
  const form = useForm<DiscoveryForm>({
    resolver: zodResolver(discoverySchema),
    defaultValues: { url: '', share: false },
  })
  const [selectedGoal, setSelectedGoal] = useState<string | null>(null)
  const [open, setOpen] = useState(true)
  const [requestedUrl, setRequestedUrl] = useState<string | null>(null)
  const selectedDomain = domains.data?.find(
    (domain) => domain.domain === sourceDomain
  )
  const resolvedUrl =
    sourceUrl ??
    selectedDomain?.url ??
    (sourceDomain ? '' : (environment?.main_url ?? ''))
  useEffect(() => {
    form.setValue('url', resolvedUrl)
  }, [resolvedUrl, form])
  const suggest = useMutation({
    ...suggestActivityGoalsMutation(),
    onSuccess: () => {
      setSelectedGoal(null)
      toast.success('Choose a goal to adapt')
    },
    onError: (error) => toast.error(errorMessage(error)),
  })
  return (
    <details
      open={open}
      onToggle={(event) => setOpen(event.currentTarget.open)}
      className="rounded-lg border p-4"
    >
      <summary className="cursor-pointer font-medium">
        Discover goals from your app
        {selectedGoal ? ` · ${selectedGoal}` : ''}
      </summary>
      <div className="mt-4 space-y-4">
        <p className="text-sm text-muted-foreground">
          Temps reads up to four public pages from its server and up to 30
          tracked event names to suggest useful questions about your visitors.
          No login credentials are used.
        </p>
        <form
          className="space-y-3"
          onSubmit={form.handleSubmit((data) => {
            setRequestedUrl(data.url)
            suggest.mutate({
              path: { project_id: projectId },
              body: {
                url: data.url,
                share_with_ai: data.share,
                environment_id: environment?.id,
              },
            })
          })}
        >
          <fieldset
            disabled={disabled || suggest.isPending}
            className="space-y-3"
          >
            <div className="space-y-2">
              <Label htmlFor="activity-website">
                Website used to suggest goals
              </Label>
              <Select
                value={
                  sourceUrl !== null ? 'custom' : (sourceDomain ?? 'primary')
                }
                onValueChange={(value) => {
                  onSourceChange(
                    value === 'custom' ? '' : null,
                    value !== 'primary' && value !== 'custom' ? value : null
                  )
                  suggest.reset()
                  setSelectedGoal(null)
                }}
              >
                <SelectTrigger id="activity-website">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="primary">
                    Temps subdomain
                    {environment?.main_url ? ` · ${environment.main_url}` : ''}
                  </SelectItem>
                  {domains.data?.map((domain) => (
                    <SelectItem key={domain.id} value={domain.domain}>
                      {domain.domain}
                    </SelectItem>
                  ))}
                  <SelectItem value="custom">Use another URL</SelectItem>
                </SelectContent>
              </Select>
              <p className="text-xs text-muted-foreground">
                This website supplies context for goal suggestions. Visitor
                activity still comes from the selected environment.
              </p>
              {domains.isError && (
                <p className="text-sm text-muted-foreground">
                  Could not load attached domains. You can use the primary URL
                  or enter another URL.
                </p>
              )}
              {sourceDomain && !domains.isPending && !selectedDomain && (
                <p role="alert" className="text-sm text-destructive">
                  The selected domain is no longer attached. Choose another
                  website.
                </p>
              )}
            </div>
            {sourceUrl !== null && (
              <div className="space-y-2">
                <Label htmlFor="activity-source-url">
                  Public application URL
                </Label>
                <Input
                  id="activity-source-url"
                  type="url"
                  placeholder="https://your-app.example"
                  {...form.register('url', {
                    onChange: (event) => {
                      onSourceChange(event.target.value, null)
                      suggest.reset()
                    },
                  })}
                />
                {form.formState.errors.url && (
                  <p role="alert" className="text-sm text-destructive">
                    {form.formState.errors.url.message}
                  </p>
                )}
              </div>
            )}
            <label className="flex items-start gap-3 text-sm">
              <Controller
                control={form.control}
                name="share"
                render={({ field }) => (
                  <Switch
                    className="mt-0.5"
                    checked={field.value}
                    onCheckedChange={field.onChange}
                    onBlur={field.onBlur}
                    ref={field.ref}
                  />
                )}
              />
              <span>
                Allow sending public page content and tracked event names to my
                AI provider to suggest goals.
              </span>
            </label>
            {form.formState.errors.share && (
              <p role="alert" className="text-sm text-destructive">
                {form.formState.errors.share.message}
              </p>
            )}
            <div className="flex flex-wrap gap-2">
              <Button
                type="submit"
                disabled={!configured || !environment || !resolvedUrl}
              >
                {suggest.isPending
                  ? 'Reading your app…'
                  : 'Suggest goals from my app'}
              </Button>
              <Button
                type="button"
                variant="ghost"
                onClick={() => {
                  setOpen(false)
                  document.getElementById('activity-context')?.focus()
                }}
              >
                Write my own goal
              </Button>
            </div>
          </fieldset>
        </form>
        {suggest.isPending && (
          <div
            role="status"
            aria-label="Reading public pages and suggesting goals"
            className="space-y-2"
          >
            <Skeleton className="h-24 w-full" />
            <Skeleton className="h-24 w-full" />
          </div>
        )}
        {suggest.isError && (
          <Alert variant="destructive">
            <AlertDescription>
              Could not read the public site or generate goals. Use the final
              public URL if it redirects, or write your own goal below. Your
              saved setup has not changed.
            </AlertDescription>
          </Alert>
        )}
        {suggest.data && (
          <div className="space-y-3">
            <p className="text-sm font-medium">Choose a starting point</p>
            <p className="break-all text-xs text-muted-foreground">
              Suggestions for {requestedUrl}
            </p>
            {suggest.data.goals.map((goal) => (
              <button
                type="button"
                key={goal.title}
                disabled={disabled}
                className="block w-full rounded-lg border p-4 text-left hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                onClick={() => {
                  setSelectedGoal(goal.title)
                  setOpen(false)
                  onSelect(goal)
                }}
              >
                <span className="block font-medium">{goal.title}</span>
                <span className="mt-1 block text-sm">{goal.goal}</span>
                <span className="mt-2 block text-xs text-muted-foreground">
                  Why this fits: {goal.rationale}
                </span>
                <span className="mt-1 block text-xs text-muted-foreground">
                  Tracking to check: {goal.missing_signals}
                </span>
              </button>
            ))}
            <details>
              <summary className="cursor-pointer text-xs text-muted-foreground">
                Public pages read ({suggest.data.pages_read.length})
              </summary>
              <ul className="mt-2 space-y-1 text-xs">
                {suggest.data.pages_read.map((page) => (
                  <li key={page} className="break-all">
                    {page}
                  </li>
                ))}
              </ul>
            </details>
          </div>
        )}
      </div>
    </details>
  )
}
