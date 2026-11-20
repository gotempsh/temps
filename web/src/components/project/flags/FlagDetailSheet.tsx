// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { EnvironmentResponse, FlagResponse } from '@/api/client'
import {
  archiveFlagMutation,
  reshuffleFlagMutation,
  setFlagEnvironmentMutation,
  updateFlagMutation,
} from '@/api/client/@tanstack/react-query.gen'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Separator } from '@/components/ui/separator'
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from '@/components/ui/sheet'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import { useMutation } from '@tanstack/react-query'
import { Archive, Loader2, RotateCcw, Shuffle } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { FlagRulesEditor } from './FlagRulesEditor'
import { FlagTestPanel } from './FlagTestPanel'
import { FlagValueField } from './FlagValueField'
import { FlagVariantsEditor } from './FlagVariantsEditor'
import {
  asFlagValueType,
  environmentOverride,
  flagErrorMessage,
  parseFlagValue,
  toEditableString,
} from './flag-value'

interface FlagDetailSheetProps {
  projectId: number
  flag: FlagResponse | null
  environments: EnvironmentResponse[]
  open: boolean
  onOpenChange: (open: boolean) => void
  onChanged: () => void
}

/**
 * Shell only. The editable body is keyed by flag key so opening a different
 * flag remounts it with fresh state — no effect syncing props into state, and
 * no flash of the previously-opened flag's values.
 */
export function FlagDetailSheet({
  projectId,
  flag,
  environments,
  open,
  onOpenChange,
  onChanged,
}: FlagDetailSheetProps) {
  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent className="w-full overflow-y-auto sm:max-w-xl">
        {flag && (
          <FlagDetailBody
            key={flag.key}
            projectId={projectId}
            flag={flag}
            environments={environments}
            onOpenChange={onOpenChange}
            onChanged={onChanged}
          />
        )}
      </SheetContent>
    </Sheet>
  )
}

interface FlagDetailBodyProps {
  projectId: number
  flag: FlagResponse
  environments: EnvironmentResponse[]
  onOpenChange: (open: boolean) => void
  onChanged: () => void
}

function FlagDetailBody({
  projectId,
  flag,
  environments,
  onOpenChange,
  onChanged,
}: FlagDetailBodyProps) {
  const valueType = asFlagValueType(flag.value_type)

  const [defaultValue, setDefaultValue] = useState(() =>
    toEditableString(flag.default_value, asFlagValueType(flag.value_type))
  )
  const [description, setDescription] = useState(flag.description ?? '')
  const [defaultError, setDefaultError] = useState<string | null>(null)
  const [exposurePercent, setExposurePercent] = useState(() =>
    String(Math.round((flag.exposure_sample_rate ?? 0) * 10000) / 100)
  )
  const [exposureError, setExposureError] = useState<string | null>(null)
  const [confirmArchive, setConfirmArchive] = useState(false)
  const [confirmReshuffle, setConfirmReshuffle] = useState(false)

  const update = useMutation({
    ...updateFlagMutation(),
    onSuccess: () => {
      toast.success('Flag updated')
      onChanged()
    },
    onError: (error) => {
      const status = (error as { status?: number } | null)?.status
      toast.error(
        status === 409
          ? 'The flag changed after you opened it. Close and reopen it, then apply your change again.'
          : flagErrorMessage(error, 'Could not update the flag')
      )
    },
  })

  const reshuffle = useMutation({
    ...reshuffleFlagMutation(),
    onSuccess: () => {
      toast.success('Rollout buckets reshuffled')
      setConfirmReshuffle(false)
      onChanged()
    },
    onError: (error) =>
      toast.error(flagErrorMessage(error, 'Could not reshuffle the flag')),
  })

  const setEnvironment = useMutation({
    ...setFlagEnvironmentMutation(),
    onSuccess: () => onChanged(),
    onError: (error) =>
      toast.error(flagErrorMessage(error, 'Could not update the environment')),
  })

  const archive = useMutation({
    ...archiveFlagMutation(),
    onSuccess: () => {
      toast.success('Flag archived')
      setConfirmArchive(false)
      onOpenChange(false)
      onChanged()
    },
    onError: (error) =>
      toast.error(flagErrorMessage(error, 'Could not archive the flag')),
  })

  const saveDefinition = () => {
    const parsed = parseFlagValue(defaultValue, valueType)
    if (!parsed.ok) {
      setDefaultError(parsed.error)
      return
    }
    setDefaultError(null)

    const percent = Number(exposurePercent)
    if (
      exposurePercent.trim() === '' ||
      !Number.isFinite(percent) ||
      percent < 0 ||
      percent > 100
    ) {
      setExposureError('Enter a percentage between 0 and 100.')
      return
    }
    setExposureError(null)

    update.mutate({
      path: { project_id: projectId, key: flag.key },
      body: {
        default_value: parsed.value,
        description: description.trim() || null,
        exposure_sample_rate: percent / 100,
        expected_updated_at: flag.updated_at,
      },
    })
  }

  const setClientVisible = (next: boolean) => {
    update.mutate({
      path: { project_id: projectId, key: flag.key },
      body: { client_visible: next, expected_updated_at: flag.updated_at },
    })
  }

  return (
    <>
      <SheetHeader>
        <SheetTitle className="flex flex-wrap items-center gap-2">
          <span className="font-mono text-sm">{flag.key}</span>
          <Badge variant="outline">{flag.value_type}</Badge>
          {flag.client_visible && <Badge variant="secondary">Client</Badge>}
        </SheetTitle>
        <SheetDescription>
          Values are set per environment. Changes take effect within seconds —
          no redeploy.
        </SheetDescription>
      </SheetHeader>

      <div className="space-y-8 py-6">
        {/* ---------------------------------------------------------- */}
        <section className="space-y-2">
          <h3 className="text-base font-semibold tracking-tight">Usage</h3>
          <div className="flex items-baseline justify-between gap-4 rounded-md border p-3">
            <div className="min-w-0 space-y-1">
              {/* Labelled for exactly what it measures. "Last evaluated"
                  alone reads as "last time anything touched this flag", which
                  would make an unused flag look alive and invite deleting a
                  live one. */}
              <p className="text-sm font-medium">Last evaluated by an app</p>
              <p className="text-xs text-muted-foreground">
                Reported by the SDK when your code actually reads this flag —
                not when the console or a deploy fetches it.
              </p>
            </div>
            <div className="shrink-0 text-sm tabular-nums">
              {flag.last_evaluated_at ? (
                <TimeAgo date={flag.last_evaluated_at} />
              ) : (
                <span className="text-muted-foreground">Never</span>
              )}
            </div>
          </div>
        </section>

        <Separator />

        {/* ---------------------------------------------------------- */}
        <section className="space-y-4">
          <h3 className="text-base font-semibold tracking-tight">Definition</h3>

          <FlagValueField
            id="sheet-default-value"
            label="Default value"
            type={valueType}
            value={defaultValue}
            onChange={(next) => {
              setDefaultValue(next)
              if (defaultError) setDefaultError(null)
            }}
            error={defaultError}
            description="Served when an environment has no value of its own, and whenever the flag is disabled."
          />

          <div className="space-y-2">
            <Label htmlFor="sheet-description">Description</Label>
            <Textarea
              id="sheet-description"
              name="sheet-description"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              rows={2}
              placeholder="What this flag controls"
            />
          </div>

          <div className="flex items-start justify-between gap-4 rounded-md border p-3">
            <div className="space-y-1">
              <Label htmlFor="sheet-client-visible">Expose to browsers</Label>
              <p className="text-xs text-muted-foreground">
                Server-only flags never reach client code.
              </p>
            </div>
            <Switch
              id="sheet-client-visible"
              checked={flag.client_visible}
              onCheckedChange={setClientVisible}
              disabled={update.isPending}
            />
          </div>

          <div className="space-y-2 rounded-md border p-3">
            <Label htmlFor="sheet-exposure-rate">
              Record exposures (% of visitors)
            </Label>
            <Input
              id="sheet-exposure-rate"
              type="number"
              min={0}
              max={100}
              step="any"
              value={exposurePercent}
              onChange={(e) => {
                setExposurePercent(e.target.value)
                if (exposureError) setExposureError(null)
              }}
              className="w-full tabular-nums sm:w-[120px]"
            />
            {exposureError ? (
              <p className="text-xs text-destructive">{exposureError}</p>
            ) : (
              <p className="text-xs text-muted-foreground">
                {flag.client_visible
                  ? 'When above 0, browsers record a flag_exposure analytics event the first time this share of visitors reads the flag on a page. Off (0) by default.'
                  : 'Applies once the flag is exposed to browsers: a flag_exposure analytics event per page for this share of visitors.'}
              </p>
            )}
          </div>

          <Button
            type="button"
            size="sm"
            variant="outline"
            onClick={saveDefinition}
            disabled={update.isPending}
          >
            {update.isPending && (
              <Loader2 className="mr-1.5 h-4 w-4 animate-spin" />
            )}
            Save definition
          </Button>
        </section>

        <Separator />

        <section>
          <FlagVariantsEditor
            // Re-seeded whenever the stored definition changes.
            key={flag.updated_at}
            projectId={projectId}
            flag={flag}
            onChanged={onChanged}
          />
        </section>

        <Separator />

        {/* ---------------------------------------------------------- */}
        <section className="space-y-4">
          <div className="space-y-1">
            <h3 className="text-base font-semibold tracking-tight">
              Environments
            </h3>
            <p className="text-sm text-muted-foreground">
              Disabling a flag makes it serve the default, ignoring any value
              and rules set here. Rules run before the value: the first one that
              matches decides.
            </p>
          </div>

          <div className="divide-y rounded-md border">
            {environments.map((environment) => (
              <div key={environment.id}>
                <EnvironmentRow
                  // Keyed on the server state as well as the id: when a
                  // mutation lands, the row remounts and re-seeds its draft
                  // from the new value instead of syncing it in an effect.
                  key={environmentRowKey(flag, environment.id)}
                  flag={flag}
                  environment={environment}
                  projectId={projectId}
                  pending={setEnvironment.isPending}
                  onSet={(body) =>
                    setEnvironment.mutate({
                      path: {
                        project_id: projectId,
                        key: flag.key,
                        environment_id: environment.id,
                      },
                      body,
                    })
                  }
                />
                <div className="px-4 pb-4">
                  <FlagRulesEditor
                    key={rulesEditorKey(flag, environment.id)}
                    projectId={projectId}
                    flag={flag}
                    environment={environment}
                    onChanged={onChanged}
                  />
                </div>
              </div>
            ))}
          </div>
        </section>

        <Separator />

        <section className="space-y-4">
          <div className="space-y-1">
            <h3 className="text-base font-semibold tracking-tight">
              Test this flag
            </h3>
            <p className="text-sm text-muted-foreground">
              See what a user would get, and why, before you ship a rule.
            </p>
          </div>
          <FlagTestPanel
            projectId={projectId}
            flag={flag}
            environments={environments}
          />
        </section>

        <Separator />

        <section className="space-y-2">
          <h3 className="text-base font-semibold tracking-tight">
            Rollout buckets
          </h3>
          <p className="text-sm text-muted-foreground">
            The same user always lands in the same percentage bucket and split
            arm. Reshuffling draws a new assignment for every rollout and split
            of this flag in every environment. Never do it during a running
            experiment.
          </p>
          <Button
            type="button"
            size="sm"
            variant="outline"
            onClick={() => setConfirmReshuffle(true)}
            disabled={reshuffle.isPending}
          >
            <Shuffle className="mr-1.5 h-4 w-4" />
            Reshuffle
          </Button>
        </section>

        <Separator />

        {/* ---------------------------------------------------------- */}
        <section className="space-y-2">
          <h3 className="text-base font-semibold tracking-tight">Archive</h3>
          <p className="text-sm text-muted-foreground">
            Archiving stops the flag being served. Apps fall back to the default
            value compiled into their own code.
          </p>
          <Button
            type="button"
            size="sm"
            variant="outline"
            onClick={() => setConfirmArchive(true)}
            disabled={archive.isPending || Boolean(flag.archived_at)}
          >
            <Archive className="mr-1.5 h-4 w-4" />
            {flag.archived_at ? 'Already archived' : 'Archive flag'}
          </Button>
        </section>
      </div>

      <AlertDialog open={confirmReshuffle} onOpenChange={setConfirmReshuffle}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Reshuffle {flag.key}?</AlertDialogTitle>
            <AlertDialogDescription>
              Users in a partial rollout or split may switch to a different
              value on their next request. Results of an experiment running on
              this flag become meaningless.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={reshuffle.isPending}>
              Cancel
            </AlertDialogCancel>
            <AlertDialogAction
              onClick={(event) => {
                event.preventDefault()
                reshuffle.mutate({
                  path: { project_id: projectId, key: flag.key },
                })
              }}
              disabled={reshuffle.isPending}
            >
              {reshuffle.isPending && (
                <Loader2 className="mr-1.5 h-4 w-4 animate-spin" />
              )}
              Reshuffle
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog open={confirmArchive} onOpenChange={setConfirmArchive}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Archive {flag.key}?</AlertDialogTitle>
            <AlertDialogDescription>
              Apps will stop receiving this flag and fall back to the default
              value in their own code. Make sure that fallback is what you want
              in production before archiving.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={archive.isPending}>
              Cancel
            </AlertDialogCancel>
            <AlertDialogAction
              onClick={(event) => {
                event.preventDefault()
                archive.mutate({
                  path: { project_id: projectId, key: flag.key },
                })
              }}
              disabled={archive.isPending}
            >
              {archive.isPending && (
                <Loader2 className="mr-1.5 h-4 w-4 animate-spin" />
              )}
              Archive
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  )
}

/**
 * Identity for one environment row, including the server state it renders.
 * Changing server state changes the key, which remounts the row and resets its
 * local draft — React's prescribed alternative to syncing props into state.
 */
function environmentRowKey(flag: FlagResponse, environmentId: number): string {
  const override = environmentOverride(flag, environmentId)
  return [
    environmentId,
    override?.enabled ?? true,
    JSON.stringify(override?.value ?? null),
    JSON.stringify(flag.default_value ?? null),
  ].join(':')
}

/** Re-seed the rules editor whenever the stored rules change. */
function rulesEditorKey(flag: FlagResponse, environmentId: number): string {
  const override = environmentOverride(flag, environmentId)
  return `${environmentId}:${override?.updated_at ?? 'none'}:${flag.updated_at}`
}

interface EnvironmentRowProps {
  flag: FlagResponse
  environment: EnvironmentResponse
  projectId: number
  pending: boolean
  onSet: (body: { value?: unknown; enabled?: boolean | null }) => void
}

function EnvironmentRow({
  flag,
  environment,
  pending,
  onSet,
}: EnvironmentRowProps) {
  const valueType = asFlagValueType(flag.value_type)
  const override = environmentOverride(flag, environment.id)
  const hasOverride = override?.value !== null && override?.value !== undefined
  const enabled = override?.enabled ?? true

  const [draft, setDraft] = useState(() =>
    toEditableString(override?.value ?? flag.default_value, valueType)
  )
  const [error, setError] = useState<string | null>(null)

  const commit = (raw: string) => {
    const parsed = parseFlagValue(raw, valueType)
    if (!parsed.ok) {
      setError(parsed.error)
      return
    }
    setError(null)
    onSet({ value: parsed.value })
  }

  return (
    <div className="space-y-3 p-4">
      <div className="flex items-center justify-between gap-3">
        <div className="min-w-0 space-y-1">
          <p className="truncate text-sm font-medium">{environment.name}</p>
          <p className="text-xs text-muted-foreground">
            {hasOverride ? 'Set for this environment' : 'Inherits the default'}
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {!enabled && <Badge variant="outline">Disabled</Badge>}
          <Switch
            aria-label={`${enabled ? 'Disable' : 'Enable'} ${flag.key} in ${environment.name}`}
            checked={enabled}
            onCheckedChange={(next) => onSet({ enabled: next })}
            disabled={pending}
          />
        </div>
      </div>

      <div className="flex items-end gap-2">
        <div className="min-w-0 flex-1">
          <FlagValueField
            id={`env-${environment.id}-value`}
            label="Value"
            type={valueType}
            value={draft}
            onChange={(next) => {
              setDraft(next)
              if (error) setError(null)
              // A switch has no blur to commit on, so write through directly.
              if (valueType === 'bool') commit(next)
            }}
            error={error}
            disabled={pending || !enabled}
          />
        </div>

        {valueType !== 'bool' && (
          <Button
            type="button"
            size="sm"
            variant="outline"
            onClick={() => commit(draft)}
            disabled={pending || !enabled}
          >
            Set
          </Button>
        )}

        {hasOverride && (
          <Button
            type="button"
            size="sm"
            variant="ghost"
            onClick={() => onSet({ value: null })}
            disabled={pending}
            title="Clear the override so this environment inherits the default"
          >
            <RotateCcw className="h-4 w-4" />
            <span className="sr-only">Clear override</span>
          </Button>
        )}
      </div>
    </div>
  )
}
