// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Where this project's spans are stored, and how much of each span may leave
 * the instance (ADR-040 fidelity + ADR-041 write mode).
 *
 * The two controls live together because they are gated on each other: a
 * Cloud-primary project must be at `queryable` fidelity, and a `queryable`
 * project cannot be lowered back to `metered` while it is Cloud-primary. Split
 * across two screens, an operator would meet each refusal without the context
 * that explains it.
 *
 * The Cloud-primary option **always renders**, including on an instance that
 * has never been linked. When it is unavailable it says what it would do, what
 * is missing, and links to the page that fixes it — the server answers with
 * `cloud_write_mode_available: false` plus a reason and a setup path in exactly
 * that state, so this never has to guess.
 */

import {
  ProjectResponse,
  type CloudTelemetryFidelity,
  type CloudTelemetryWriteMode,
  type ProjectCloudTelemetryResponse,
} from '@/api/client'
import {
  getCloudTelemetryStatusQueryKey,
  getProjectCloudTelemetryOptions,
  getProjectCloudTelemetryQueryKey,
  updateProjectCloudTelemetryMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { problemDetail, problemSetupPath } from '@/lib/api-problem'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Label } from '@/components/ui/label'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import { Skeleton } from '@/components/ui/skeleton'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { formatDistanceToNow } from 'date-fns'
import {
  AlertTriangle,
  ArrowRight,
  CloudUpload,
  HardDrive,
  Info,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'

interface TelemetrySettingsProps {
  project: ProjectResponse
}

const WRITE_MODE_COPY: Record<
  CloudTelemetryWriteMode,
  { title: string; description: string; icon: typeof HardDrive }
> = {
  local: {
    title: 'Store spans on this instance',
    description:
      'Spans are written to this instance’s span store first, then optionally mirrored to Temps Cloud. This is the default and what every project did before Cloud-primary writes existed.',
    icon: HardDrive,
  },
  cloud: {
    title: 'Write spans to Temps Cloud',
    description:
      'Spans go straight to this instance’s durable telemetry queue and are shipped to Temps Cloud. No span for this project is stored on this instance — which is how a local span store stops being necessary. Traces are then read back from Cloud.',
    icon: CloudUpload,
  },
}

const FIDELITY_COPY: Record<
  CloudTelemetryFidelity,
  { title: string; description: string }
> = {
  metered: {
    title: 'Metered',
    description:
      'Only pseudonymised counters and timings leave this instance. Nothing readable — no span names, no attributes — so these spans cannot be searched in Cloud.',
  },
  queryable: {
    title: 'Queryable',
    description:
      'Span names, timings and the allowlisted attributes leave this instance, so traces can be searched and read in Cloud.',
  },
}

function WriteModeBadge({
  settings,
}: {
  settings: ProjectCloudTelemetryResponse
}) {
  if (settings.effective_write_mode === settings.write_mode) {
    return (
      <Badge
        variant={settings.write_mode === 'cloud' ? 'default' : 'secondary'}
      >
        {settings.write_mode === 'cloud' ? 'Cloud-primary' : 'Local'}
      </Badge>
    )
  }
  // Intent and reality disagree — a quota or credential fallback. Showing only
  // the declared mode here would tell the operator their spans are somewhere
  // they are not.
  return <Badge variant="destructive">Falling back to local storage</Badge>
}

export function TelemetrySettings({ project }: TelemetrySettingsProps) {
  usePageTitle(`Telemetry storage - ${project.name}`)
  const queryClient = useQueryClient()

  const {
    data: settings,
    isPending,
    isError,
    error,
    refetch,
  } = useQuery(
    getProjectCloudTelemetryOptions({ path: { project_id: project.id } })
  )

  const [writeMode, setWriteMode] = useState<CloudTelemetryWriteMode>('local')
  const [fidelity, setFidelity] = useState<CloudTelemetryFidelity>('metered')

  useEffect(() => {
    if (!settings) return
    setWriteMode(settings.write_mode)
    setFidelity(settings.fidelity)
  }, [settings])

  const save = useMutation({
    ...updateProjectCloudTelemetryMutation(),
    onSuccess: (updated) => {
      queryClient.setQueryData(
        getProjectCloudTelemetryQueryKey({ path: { project_id: project.id } }),
        updated
      )
      void queryClient.invalidateQueries({
        queryKey: getCloudTelemetryStatusQueryKey(),
      })
      toast.success(
        updated.write_mode === 'cloud'
          ? 'This project’s spans now go to Temps Cloud. They are no longer stored on this instance.'
          : 'This project’s spans are stored on this instance.'
      )
    },
    onError: (mutationError) => {
      // The refusal names the one missing prerequisite. Surfacing a generic
      // failure here would leave the operator guessing between four unrelated
      // fixes.
      toast.error(
        problemDetail(
          mutationError,
          'Could not change this project’s telemetry storage.'
        ),
        {
          action: problemSetupPath(mutationError)
            ? {
                label: 'Fix this',
                onClick: () => {
                  window.location.href = problemSetupPath(
                    mutationError
                  ) as string
                },
              }
            : undefined,
          duration: 12_000,
        }
      )
    },
  })

  if (isPending) {
    return (
      <div className="mx-auto w-full max-w-3xl space-y-4 px-4 py-8 sm:px-6">
        <Skeleton className="h-8 w-64" />
        <Skeleton className="h-48 w-full" />
        <Skeleton className="h-40 w-full" />
      </div>
    )
  }

  if (isError || !settings) {
    return (
      <div className="mx-auto w-full max-w-3xl px-4 py-8 sm:px-6">
        <Alert variant="destructive">
          <AlertTriangle className="h-4 w-4" />
          <AlertTitle>Telemetry storage settings unavailable</AlertTitle>
          <AlertDescription className="space-y-3">
            <p>
              {problemDetail(
                error,
                'This instance could not report where this project’s spans are stored.'
              )}
            </p>
            <Button size="sm" variant="outline" onClick={() => void refetch()}>
              Try again
            </Button>
          </AlertDescription>
        </Alert>
      </div>
    )
  }

  const cloudAvailable = settings.cloud_write_mode_available
  const dirty =
    writeMode !== settings.write_mode || fidelity !== settings.fidelity
  // Lowering fidelity while Cloud-primary is refused by the server; saying so
  // here is cheaper for the operator than a round trip that ends in an error.
  const downgradeBlocked = writeMode === 'cloud' && fidelity === 'metered'

  return (
    <div className="mx-auto w-full max-w-3xl space-y-6 px-4 py-8 sm:px-6">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h2 className="text-2xl font-semibold tracking-tight">
            Telemetry storage
          </h2>
          <p className="mt-1 text-sm text-muted-foreground">
            Where {project.name}’s spans are stored, and how much of each span
            may leave this instance.
          </p>
        </div>
        <WriteModeBadge settings={settings} />
      </div>

      {settings.effective_write_mode !== settings.write_mode && (
        <Alert variant="destructive">
          <AlertTriangle className="h-4 w-4" />
          <AlertTitle>
            Spans are not going where this project’s settings say
          </AlertTitle>
          <AlertDescription>
            {settings.effective_reason_message ??
              'Temps Cloud is not accepting this project’s spans, so they are being stored on this instance instead.'}{' '}
            The setting below is unchanged and takes effect again automatically
            once Cloud accepts.
          </AlertDescription>
        </Alert>
      )}

      {settings.queued_spans > 0 && (
        <Alert>
          <Info className="h-4 w-4" />
          <AlertTitle>
            {settings.queued_spans.toLocaleString()} span
            {settings.queued_spans === 1 ? '' : 's'} waiting to reach Temps
            Cloud
          </AlertTitle>
          <AlertDescription>
            These are durably queued on this instance and survive a restart.
            They are not readable in Traces until Cloud accepts them.
          </AlertDescription>
        </Alert>
      )}

      {settings.dead_lettered_spans > 0 && (
        <Alert variant="destructive">
          <AlertTriangle className="h-4 w-4" />
          <AlertTitle>
            {settings.dead_lettered_spans.toLocaleString()} span
            {settings.dead_lettered_spans === 1 ? '' : 's'} were never delivered
            to Temps Cloud
          </AlertTitle>
          <AlertDescription>
            <p>
              Delivery was retried until it gave up. These spans are not in
              Traces and will not be retried automatically.
              {settings.last_dead_letter_at
                ? ` Most recently on ${new Date(settings.last_dead_letter_at).toLocaleString()}.`
                : ''}
            </p>
            {settings.last_dead_letter_error && (
              <p className="mt-1 font-mono text-xs break-all">
                {settings.last_dead_letter_error}
              </p>
            )}
          </AlertDescription>
        </Alert>
      )}

      {/* ── Write mode ─────────────────────────────────────────────── */}
      <Card>
        <CardHeader>
          <CardTitle className="text-base">Where spans are stored</CardTitle>
          <CardDescription>
            This decides whether this project’s spans exist on this machine at
            all.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <RadioGroup
            value={writeMode}
            onValueChange={(value) =>
              setWriteMode(value as CloudTelemetryWriteMode)
            }
            className="space-y-3"
          >
            {(['local', 'cloud'] as const).map((mode) => {
              const copy = WRITE_MODE_COPY[mode]
              const disabled = mode === 'cloud' && !cloudAvailable
              return (
                <div
                  key={mode}
                  className={`flex gap-3 rounded-lg border p-4 ${
                    disabled ? 'opacity-70' : ''
                  }`}
                >
                  <RadioGroupItem
                    value={mode}
                    id={`write-mode-${mode}`}
                    disabled={disabled}
                    className="mt-1"
                  />
                  <div className="min-w-0 flex-1 space-y-1">
                    <Label
                      htmlFor={`write-mode-${mode}`}
                      className="flex items-center gap-2 text-sm font-medium"
                    >
                      <copy.icon className="size-4 text-muted-foreground" />
                      {copy.title}
                    </Label>
                    <p className="text-xs leading-5 text-muted-foreground">
                      {copy.description}
                    </p>
                    {disabled && settings.reason && (
                      // Never render nothing: the option stays visible and
                      // explains itself instead of disappearing.
                      <div className="mt-2 rounded-md border border-dashed bg-muted/40 p-3">
                        <p className="text-xs leading-5 text-muted-foreground">
                          {settings.reason}
                        </p>
                        {settings.setup_path && (
                          <Button
                            asChild
                            size="sm"
                            variant="outline"
                            className="mt-2 gap-1.5"
                          >
                            <Link to={settings.setup_path}>
                              Set this up
                              <ArrowRight className="size-3.5" />
                            </Link>
                          </Button>
                        )}
                      </div>
                    )}
                  </div>
                </div>
              )
            })}
          </RadioGroup>

          {writeMode === 'cloud' && settings.write_mode !== 'cloud' && (
            <Alert>
              <AlertTriangle className="h-4 w-4" />
              <AlertTitle>What changes when you save this</AlertTitle>
              <AlertDescription>
                New spans stop being written to this instance. Spans already
                stored here stay readable until they age out of retention, and
                traces from after the switch are read back from Temps Cloud. A
                query that crosses the switch is answered from one side and told
                you where it was cut — never silently merged.
              </AlertDescription>
            </Alert>
          )}
        </CardContent>
      </Card>

      {/* ── Fidelity ───────────────────────────────────────────────── */}
      <Card>
        <CardHeader>
          <CardTitle className="text-base">
            How much of each span leaves this instance
          </CardTitle>
          <CardDescription>
            Applies to spans sent to Temps Cloud, whether as a mirror or as this
            project’s primary store.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <RadioGroup
            value={fidelity}
            onValueChange={(value) =>
              setFidelity(value as CloudTelemetryFidelity)
            }
            className="space-y-3"
          >
            {(['metered', 'queryable'] as const).map((tier) => {
              const copy = FIDELITY_COPY[tier]
              return (
                <div key={tier} className="flex gap-3 rounded-lg border p-4">
                  <RadioGroupItem
                    value={tier}
                    id={`fidelity-${tier}`}
                    className="mt-1"
                  />
                  <div className="min-w-0 flex-1 space-y-1">
                    <Label
                      htmlFor={`fidelity-${tier}`}
                      className="text-sm font-medium"
                    >
                      {copy.title}
                    </Label>
                    <p className="text-xs leading-5 text-muted-foreground">
                      {copy.description}
                    </p>
                  </div>
                </div>
              )
            })}
          </RadioGroup>

          {settings.attribute_allowlist.length > 0 && (
            <div className="rounded-md border bg-muted/30 p-3">
              <p className="text-xs font-medium">Attributes allowed to leave</p>
              <div className="mt-2 flex flex-wrap gap-1.5">
                {settings.attribute_allowlist.map((key) => (
                  <Badge
                    key={key}
                    variant="outline"
                    className="font-mono text-[11px]"
                  >
                    {key}
                  </Badge>
                ))}
              </div>
            </div>
          )}

          {downgradeBlocked && (
            <Alert variant="destructive">
              <AlertTriangle className="h-4 w-4" />
              <AlertTitle>
                Metered fidelity cannot be combined with Cloud-primary writes
              </AlertTitle>
              <AlertDescription>
                Metered spans are pseudonymised placeholders that cannot be read
                back, and a Cloud-primary project stores nothing here — this
                project’s traces would exist nowhere. Choose “Store spans on
                this instance”, or keep fidelity at Queryable.
              </AlertDescription>
            </Alert>
          )}
        </CardContent>
        <CardFooter className="justify-end gap-2">
          {dirty && (
            <Button
              variant="ghost"
              onClick={() => {
                setWriteMode(settings.write_mode)
                setFidelity(settings.fidelity)
              }}
              disabled={save.isPending}
            >
              Cancel
            </Button>
          )}
          <Button
            onClick={() =>
              save.mutate({
                path: { project_id: project.id },
                body: { fidelity, write_mode: writeMode },
              })
            }
            disabled={!dirty || downgradeBlocked || save.isPending}
          >
            {save.isPending ? 'Saving…' : 'Save changes'}
          </Button>
        </CardFooter>
      </Card>

      {/* ── History ────────────────────────────────────────────────── */}
      {(settings.intervals.length > 0 || settings.gap_windows.length > 0) && (
        <Card>
          <CardHeader>
            <CardTitle className="text-base">Storage history</CardTitle>
            <CardDescription>
              Where this project’s spans actually went, which is what decides
              which store answers a query for a given time range.
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            {settings.gap_windows.length > 0 && (
              <div className="space-y-2">
                <p className="text-xs font-medium text-destructive">
                  Spans not captured anywhere
                </p>
                {settings.gap_windows.map((gap) => (
                  <div
                    key={`${gap.started_at}-${gap.ended_at}`}
                    className="rounded-md border border-destructive/40 bg-destructive/5 p-3 text-xs leading-5"
                  >
                    <p className="font-medium">
                      {gap.dropped_spans.toLocaleString()} span
                      {gap.dropped_spans === 1 ? '' : 's'} lost between{' '}
                      {new Date(gap.started_at).toLocaleString()} and{' '}
                      {new Date(gap.ended_at).toLocaleString()}
                    </p>
                    <p className="mt-1 text-muted-foreground">{gap.message}</p>
                  </div>
                ))}
              </div>
            )}

            <div className="overflow-hidden rounded-md border">
              {settings.intervals.map((interval) => (
                <div
                  key={`${interval.mode}-${interval.effective_from}`}
                  className="flex flex-col gap-1 border-b p-3 last:border-b-0 sm:flex-row sm:items-center sm:justify-between"
                >
                  <div className="min-w-0">
                    <p className="text-sm font-medium">
                      {interval.mode === 'cloud'
                        ? 'Temps Cloud'
                        : 'This instance'}
                    </p>
                    <p className="text-xs text-muted-foreground">
                      {interval.message}
                    </p>
                  </div>
                  <p className="shrink-0 text-xs text-muted-foreground">
                    {new Date(interval.effective_from).toLocaleString()} —{' '}
                    {interval.effective_to
                      ? new Date(interval.effective_to).toLocaleString()
                      : `now (${formatDistanceToNow(new Date(interval.effective_from), { addSuffix: true })})`}
                  </p>
                </div>
              ))}
            </div>
          </CardContent>
        </Card>
      )}
    </div>
  )
}
