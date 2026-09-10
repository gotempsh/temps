// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { CopyButton } from '@/components/ui/copy-button'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { UpdateNowDialog } from '@/components/alerts/UpdateNowDialog'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  useCheckForUpdate,
  useSelfUpdateCapability,
} from '@/hooks/useSelfUpdate'
import { useSettings, useUpdateSettings } from '@/hooks/useSettings'
import { useUpdateStatus } from '@/hooks/useUpdateStatus'
import {
  AlertTriangle,
  ArrowUpCircle,
  CheckCircle2,
  Info,
  Loader2,
  RefreshCw,
  Terminal,
  XCircle,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'

/** Channels the server knows how to track, worst-to-best stability last. */
const CHANNELS = [
  {
    value: 'stable',
    label: 'Stable',
    hint: 'Tagged releases only. Recommended for production.',
  },
  {
    value: 'beta',
    label: 'Beta',
    hint: 'Pre-release cuts plus stable. Excludes nightly builds.',
  },
  {
    value: 'nightly',
    label: 'Nightly',
    hint: 'Automated daily builds from main. Least stable.',
  },
] as const

/** Sentinel for "no explicit channel — follow the running version's tag". */
const INHERIT = '__inherit__'

/**
 * Version and updates page.
 *
 * One place to answer "what am I running, is there something newer, and can I
 * install it from here" — which previously required reading a banner, the
 * release page and the host's service manager.
 */
export function VersionPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const { data: settings } = useSettings()
  const updateSettings = useUpdateSettings()
  const { data: capability, isPending } = useSelfUpdateCapability()
  const { data: status } = useUpdateStatus()
  const checkForUpdate = useCheckForUpdate()
  const [updateOpen, setUpdateOpen] = useState(false)

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Version' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('Version')

  const selfUpdate = settings?.self_update
  const enabled = selfUpdate?.enabled ?? true
  // A launch flag beats the database, so the toggle is inert (and says so)
  // rather than pretending to control something it doesn't.
  const overriddenByFlag = capability?.blocker === 'disabled_by_flag'

  const saveSelfUpdate = async (
    patch: { enabled?: boolean; channel?: string | null },
    message: string
  ) => {
    try {
      await updateSettings.mutateAsync({
        self_update: {
          enabled: patch.enabled ?? enabled,
          channel:
            patch.channel !== undefined
              ? patch.channel
              : (selfUpdate?.channel ?? null),
        },
      })
      toast.success(message)
    } catch {
      // useUpdateSettings surfaces the failure toast.
    }
  }

  const latest = checkForUpdate.data
  const canApply = Boolean(capability?.can_apply && capability?.allowed)

  return (
    <div className="space-y-6">
      <Card>
        <CardHeader>
          <CardTitle>Version</CardTitle>
          <CardDescription>
            What this server is running, and what is available on its channel.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          {isPending ? (
            <div className="space-y-2">
              <Skeleton className="h-5 w-56" />
              <Skeleton className="h-4 w-72" />
            </div>
          ) : (
            <>
              <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
                <div className="min-w-0 space-y-1">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="font-mono text-lg">
                      {capability?.current_version ?? 'unknown'}
                    </span>
                    <Badge variant="secondary">{capability?.channel}</Badge>
                    {!capability?.channel_is_pinned && (
                      <span className="text-xs text-muted-foreground">
                        (from the installed version)
                      </span>
                    )}
                  </div>
                  <p className="text-sm text-muted-foreground">
                    Managed by{' '}
                    <span className="font-mono">{capability?.supervisor}</span>{' '}
                    ·{' '}
                    {capability?.restart_mode === 'manual'
                      ? 'updates install but do not restart temps'
                      : 'updates restart temps automatically'}
                  </p>
                </div>
                <Button
                  variant="outline"
                  onClick={() => checkForUpdate.mutate({ body: undefined })}
                  disabled={checkForUpdate.isPending}
                  className="shrink-0"
                >
                  {checkForUpdate.isPending ? (
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  ) : (
                    <RefreshCw className="mr-2 h-4 w-4" />
                  )}
                  Check for updates
                </Button>
              </div>

              {capability?.binary_path && (
                <p className="text-xs text-muted-foreground">
                  Binary{' '}
                  <span className="font-mono">{capability.binary_path}</span>
                </p>
              )}

              <UpdateAvailability
                latestVersion={
                  latest?.latest_version ?? status?.latest_version ?? null
                }
                updateAvailable={Boolean(
                  latest?.update_available ?? status?.update_available
                )}
                currentVersion={capability?.current_version ?? null}
                releaseUrl={latest?.release_url ?? status?.release_url ?? null}
                checked={Boolean(latest)}
                error={problemDetail(checkForUpdate.error)}
                canApply={canApply}
                onUpdate={() => setUpdateOpen(true)}
              />
            </>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Release channel</CardTitle>
          <CardDescription>
            Which releases this server tracks. Changing it takes effect on the
            next check.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          <Select
            value={selfUpdate?.channel ?? INHERIT}
            onValueChange={(value) =>
              saveSelfUpdate(
                { channel: value === INHERIT ? null : value },
                value === INHERIT
                  ? 'Channel now follows the installed version'
                  : `Now tracking the ${value} channel`
              )
            }
          >
            <SelectTrigger className="w-full sm:w-[320px]">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={INHERIT}>
                Follow the installed version ({capability?.channel ?? 'auto'})
              </SelectItem>
              {CHANNELS.map((channel) => (
                <SelectItem key={channel.value} value={channel.value}>
                  {channel.label} — {channel.hint}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>

          {/* Switching to a more stable channel usually means the newest
              release there is OLDER than what is running, which looks like a
              bug unless we say so first. */}
          <p className="flex gap-2 text-sm text-muted-foreground">
            <Info className="mt-0.5 h-4 w-4 shrink-0" />
            <span>
              Moving to a more stable channel does not downgrade this server.
              Its newest release may be older than what you are running, in
              which case nothing is offered until that channel catches up.
            </span>
          </p>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Updating from the console</CardTitle>
          <CardDescription>
            Whether admins can install a release from here. The banner and the
            manual command are shown either way.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="flex items-center justify-between gap-4">
            <div className="space-y-1">
              <Label htmlFor="self-update-enabled">
                Allow updates from the console
              </Label>
              <p className="text-sm text-muted-foreground">
                Requires the <span className="font-mono">platform:update</span>{' '}
                permission. Every attempt is audited.
              </p>
            </div>
            <Switch
              id="self-update-enabled"
              checked={enabled && !overriddenByFlag}
              disabled={overriddenByFlag || updateSettings.isPending}
              onCheckedChange={(next) =>
                saveSelfUpdate(
                  { enabled: next },
                  next
                    ? 'Updates can now be applied from the console'
                    : 'Console updates disabled'
                )
              }
            />
          </div>

          {overriddenByFlag && (
            <p className="flex gap-2 rounded border border-amber-300 bg-amber-50 p-2 text-sm text-amber-900 dark:border-amber-900/50 dark:bg-amber-950/30 dark:text-amber-200">
              <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
              <span>
                This server was started with{' '}
                <span className="font-mono">--disable-self-update</span>, which
                overrides this setting. Remove the flag and restart temps to
                allow console updates.
              </span>
            </p>
          )}

          {capability && !capability.can_apply && !overriddenByFlag && (
            <div className="space-y-2 rounded border bg-muted/40 p-2 text-sm">
              <p className="flex gap-2">
                <Info className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground" />
                <span>{capability.reason}</span>
              </p>
              <ManualCommand command={capability.manual_command} />
            </div>
          )}

          {capability?.caveat && (
            <p className="flex gap-2 rounded border border-amber-300 bg-amber-50 p-2 text-sm text-amber-900 dark:border-amber-900/50 dark:bg-amber-950/30 dark:text-amber-200">
              <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
              <span>{capability.caveat}</span>
            </p>
          )}
        </CardContent>
      </Card>

      {capability?.last_attempt && (
        <LastAttemptCard attempt={capability.last_attempt} />
      )}

      <UpdateNowDialog
        open={updateOpen}
        onOpenChange={setUpdateOpen}
        currentVersion={capability?.current_version}
        latestVersion={latest?.latest_version ?? status?.latest_version}
      />
    </div>
  )
}

/**
 * Pull the human-readable reason out of a failed request.
 *
 * The API returns RFC 7807 Problem Details, and `String(err)` on that object
 * renders "[object Object]" — i.e. the user is told something failed but never
 * why, which is exactly the state this page exists to avoid.
 */
function problemDetail(error: unknown): string | null {
  if (!error) return null
  const problem = error as { detail?: string; title?: string; message?: string }
  return (
    problem.detail ??
    problem.title ??
    problem.message ??
    'The release check failed.'
  )
}

function ManualCommand({ command }: { command: string }) {
  return (
    <div className="flex items-center gap-2">
      <Terminal className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
      <code className="min-w-0 flex-1 truncate font-mono text-xs">
        {command}
      </code>
      <CopyButton value={command} minimal />
    </div>
  )
}

function UpdateAvailability({
  latestVersion,
  updateAvailable,
  currentVersion,
  releaseUrl,
  checked,
  error,
  canApply,
  onUpdate,
}: {
  latestVersion: string | null
  updateAvailable: boolean
  currentVersion: string | null
  releaseUrl: string | null
  checked: boolean
  error: string | null
  canApply: boolean
  onUpdate: () => void
}) {
  if (error) {
    return (
      <p className="flex gap-2 rounded border border-destructive/40 bg-destructive/10 p-2 text-sm text-destructive">
        <XCircle className="mt-0.5 h-4 w-4 shrink-0" />
        <span>{error}</span>
      </p>
    )
  }

  if (updateAvailable && latestVersion) {
    return (
      <div className="flex flex-col gap-3 rounded border border-blue-200 bg-blue-50/60 p-3 dark:border-blue-900/50 dark:bg-blue-950/20 sm:flex-row sm:items-center sm:justify-between">
        <p className="flex gap-2 text-sm text-blue-900 dark:text-blue-200">
          <ArrowUpCircle className="mt-0.5 h-4 w-4 shrink-0" />
          <span>
            <strong className="font-mono">{latestVersion}</strong> is available
            {currentVersion && (
              <>
                {' '}
                (you have <span className="font-mono">{currentVersion}</span>)
              </>
            )}
            {releaseUrl && (
              <>
                {' · '}
                <a
                  href={releaseUrl}
                  target="_blank"
                  rel="noreferrer"
                  className="underline underline-offset-2"
                >
                  Release notes
                </a>
              </>
            )}
          </span>
        </p>
        {canApply && (
          <Button onClick={onUpdate} className="shrink-0">
            Update now
          </Button>
        )}
      </div>
    )
  }

  // Distinguish "we asked and there is nothing" from "we haven't asked yet",
  // so a stale page never reads as a fresh all-clear.
  return (
    <p className="flex gap-2 text-sm text-muted-foreground">
      <CheckCircle2 className="mt-0.5 h-4 w-4 shrink-0" />
      <span>
        {checked
          ? `Up to date${latestVersion ? ` — the newest release on this channel is ${latestVersion}` : ''}.`
          : 'No newer release has been found. The server re-checks periodically, or check now.'}
      </span>
    </p>
  )
}

function LastAttemptCard({
  attempt,
}: {
  attempt: NonNullable<
    ReturnType<typeof useSelfUpdateCapability>['data']
  >['last_attempt']
}) {
  if (!attempt) return null

  const tone =
    attempt.status === 'succeeded'
      ? 'text-green-700 dark:text-green-400'
      : attempt.status === 'failed'
        ? 'text-destructive'
        : 'text-amber-700 dark:text-amber-400'

  const label =
    attempt.status === 'succeeded'
      ? 'Succeeded'
      : attempt.status === 'failed'
        ? 'Failed'
        : attempt.status === 'installed_pending_restart'
          ? 'Installed — restart temps to apply'
          : 'In progress'

  return (
    <Card>
      <CardHeader>
        <CardTitle>Last update attempt</CardTitle>
      </CardHeader>
      <CardContent className="space-y-2 text-sm">
        <p className={tone}>{label}</p>
        <p className="text-muted-foreground">
          <span className="font-mono">{attempt.from_version}</span>
          {attempt.to_version && (
            <>
              {' → '}
              <span className="font-mono">{attempt.to_version}</span>
            </>
          )}
          {' · '}
          {new Date(attempt.started_at).toLocaleString()}
        </p>
        {attempt.error && <p className="text-destructive">{attempt.error}</p>}
        {attempt.previous_binary_path && (
          <p className="text-xs text-muted-foreground">
            Previous binary kept at{' '}
            <span className="font-mono">{attempt.previous_binary_path}</span>
          </p>
        )}
      </CardContent>
    </Card>
  )
}
