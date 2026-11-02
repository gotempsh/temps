// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  createEnvironmentVariable,
  deleteEnvironmentVariable,
  updateEnvironmentVariable,
  type EnvironmentResponse,
  type EnvironmentVariableResponse,
} from '@/api/client'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { CopyButton } from '@/components/ui/copy-button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { Textarea } from '@/components/ui/textarea'
import { getEnvVarValue } from '@/lib/resolved-env-vars'
import {
  applyRawEnvChanges,
  mapWithConcurrency,
  parseEnvText,
  planRawEnvChanges,
  rawEditorVariables,
  rawEnvChangeKey,
  rawEnvRemovals,
  serializeEnvText,
  summarizeKeys,
  type RawEnvChange,
} from '@/lib/raw-env-editor'
import type { VariableChangeScope } from '@/lib/variable-change-redeploy'
import { extractProblemDetails, getErrorMessage } from '@/utils/errorHandling'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Loader2 } from 'lucide-react'
import { useMemo, useState } from 'react'
import { toast } from 'sonner'

/** Parallel reveals; each one is a separate audited request. */
const REVEAL_CONCURRENCY = 4

function ChangeRow({
  change,
  environmentName,
}: {
  change: RawEnvChange
  environmentName: string
}) {
  const label = {
    create: 'Add',
    update: 'Change',
    delete: 'Delete',
    unbind: `Remove from ${environmentName}`,
    blocked: 'Not applied',
  }[change.type]
  const notes: string[] = []
  if (change.type === 'update') {
    if (change.replacesSecret) notes.push('Replaces the write-only secret.')
    if (change.alsoAffects.length > 0)
      notes.push(`Also changes ${change.alsoAffects.join(', ')}.`)
  }
  if (change.type === 'delete')
    notes.push('Only bound to this environment, so it is deleted.')
  if (change.type === 'blocked') notes.push(change.reason)
  return (
    <li className="flex flex-col gap-1 border-b py-2 last:border-b-0">
      <div className="flex items-center gap-2">
        <Badge
          variant={
            change.type === 'create'
              ? 'success-subtle'
              : change.type === 'update'
                ? 'neutral-subtle'
                : change.type === 'blocked'
                  ? 'warning-subtle'
                  : 'destructive-subtle'
          }
        >
          {label}
        </Badge>
        <span className="font-mono text-sm break-all">
          {rawEnvChangeKey(change)}
        </span>
      </div>
      {notes.map((note) => (
        <p key={note} className="text-xs text-muted-foreground">
          {note}
        </p>
      ))}
    </li>
  )
}

export interface RawEnvironmentVariablesEditorProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  projectId: number
  variables: EnvironmentVariableResponse[]
  environments: EnvironmentResponse[]
  /** Environment the page is filtered to; the editor starts there. */
  initialEnvironmentId: number | null
  /** Called after changes were sent, with what to tell the user. */
  onApplied: (title: string, scope: VariableChangeScope) => void
}

/**
 * Shows one environment's variables as `.env` text and applies the edited
 * text back as individual variable changes.
 *
 * Values are read through the same audited per-variable reveal the table's
 * eye buttons use, so opening the editor is equivalent to revealing each
 * value: one audit record and one permission check per variable. Secrets are
 * never revealed and never part of the text.
 */
export function RawEnvironmentVariablesEditor({
  open,
  onOpenChange,
  projectId,
  variables,
  environments,
  initialEnvironmentId,
  onApplied,
}: RawEnvironmentVariablesEditorProps) {
  const queryClient = useQueryClient()
  const [chosenEnvironmentId, setChosenEnvironmentId] = useState<number | null>(
    null
  )
  const [draft, setDraft] = useState<{ scope: string; text: string } | null>(
    null
  )
  const [reviewing, setReviewing] = useState(false)
  const [confirmingRemovals, setConfirmingRemovals] = useState(false)

  const environment =
    environments.find(
      (env) => env.id === (chosenEnvironmentId ?? initialEnvironmentId)
    ) ?? environments[0]

  const scoped = useMemo(
    () => (environment ? rawEditorVariables(variables, environment) : []),
    [variables, environment]
  )
  const revealable = useMemo(
    () => scoped.filter(({ variable }) => !variable.is_secret),
    [scoped]
  )
  const secrets = scoped.filter(({ variable }) => variable.is_secret)
  // Keyed by environment only: the variables list is polled, and a change to
  // it (another user, the CLI) must neither repeat these audited reveals nor
  // reset the draft. A variable added meanwhile is not in the text and is
  // left unchanged.
  const revealQueryKey = ['raw-env-editor', projectId, environment?.id ?? null]

  const reveal = useQuery({
    queryKey: revealQueryKey,
    // Closing the editor cancels the query: stop starting audited reveals
    // whose values nothing will show.
    queryFn: async ({ signal }) => {
      const values = await mapWithConcurrency(
        revealable,
        REVEAL_CONCURRENCY,
        async ({ variable }) =>
          [
            variable.id,
            await getEnvVarValue(projectId, variable.key, variable.id),
          ] as const,
        signal
      )
      return new Map<number, string>(values)
    },
    enabled: open && Boolean(environment),
    // Revealing is audited: never repeat it on focus or reconnect, and drop
    // the plaintext as soon as nothing shows it.
    staleTime: Infinity,
    gcTime: 0,
    retry: false,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  })

  // A refusal is about the caller's role, not a transient failure: say so
  // instead of offering a retry that is refused the same way.
  const revealForbidden =
    reveal.isError && extractProblemDetails(reveal.error)?.status === 403

  const baselineText = useMemo(
    () =>
      reveal.data
        ? serializeEnvText(
            revealable.flatMap(({ variable }) => {
              const value = reveal.data.get(variable.id)
              return value === undefined ? [] : [{ key: variable.key, value }]
            })
          )
        : '',
    [reveal.data, revealable]
  )
  const draftScope = `${environment?.id ?? 'none'}`
  const text = draft?.scope === draftScope ? draft.text : baselineText
  const parsed = useMemo(() => parseEnvText(text), [text])
  const changes = useMemo(
    () =>
      environment && reveal.data
        ? planRawEnvChanges({
            scoped,
            revealed: reveal.data,
            entries: parsed.entries,
            environment,
          })
        : [],
    [environment, reveal.data, scoped, parsed.entries]
  )
  const applicable = changes.filter((change) => change.type !== 'blocked')
  const removals = rawEnvRemovals(applicable)
  const removedKeys = [...removals.deleted, ...removals.unbound]

  const close = () => {
    setDraft(null)
    setReviewing(false)
    setConfirmingRemovals(false)
    setChosenEnvironmentId(null)
    queryClient.removeQueries({ queryKey: ['raw-env-editor', projectId] })
    onOpenChange(false)
  }

  const apply = useMutation({
    mutationFn: async () => {
      if (!environment) return { applied: 0, failures: [], scope: [] }
      const scope = new Set<number>([environment.id])
      const { applied, failures } = await applyRawEnvChanges(
        applicable,
        async (change) => {
          if (change.type === 'create') {
            await createEnvironmentVariable({
              path: { project_id: projectId },
              body: {
                key: change.key,
                value: change.value,
                environment_ids: [environment.id],
                include_in_preview: false,
              },
              throwOnError: true,
            })
          } else if (change.type === 'update' || change.type === 'unbind') {
            const { variable } = change
            await updateEnvironmentVariable({
              path: { project_id: projectId, var_id: variable.id },
              body: {
                key: variable.key,
                value: change.type === 'update' ? change.value : undefined,
                environment_ids:
                  change.type === 'update'
                    ? variable.environments.map((env) => env.id)
                    : change.remainingEnvironmentIds,
                include_in_preview: variable.include_in_preview,
              },
              throwOnError: true,
            })
            variable.environments.forEach((env) => scope.add(env.id))
          } else if (change.type === 'delete') {
            await deleteEnvironmentVariable({
              path: { project_id: projectId, var_id: change.variable.id },
              throwOnError: true,
            })
          }
        },
        (error) => getErrorMessage(error, 'Request failed')
      )
      return { applied, failures, scope: [...scope] }
    },
    onSuccess: ({ applied, failures, scope }) => {
      if (applied > 0) {
        onApplied(
          `Applied ${applied} change${applied !== 1 ? 's' : ''} to ${environment?.name}`,
          scope
        )
      }
      if (failures.length > 0) {
        toast.error(
          `${failures.length} change${failures.length !== 1 ? 's' : ''} could not be applied`,
          {
            description: failures
              .map((failure) => `${failure.key}: ${failure.message}`)
              .join('\n'),
          }
        )
        // Keep the text so the failed lines can be fixed and applied again;
        // lines that did apply plan as no-op or idempotent updates.
        setReviewing(false)
        setConfirmingRemovals(false)
        return
      }
      close()
    },
  })

  // The only path to the mutation. Removals reach it through the delete
  // confirmation; a change set without removals makes no destructive call.
  const runApply = () => apply.mutate()

  const canReview =
    Boolean(reveal.data) && parsed.errors.length === 0 && changes.length > 0

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next && !apply.isPending) close()
      }}
    >
      <DialogContent className="sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>Edit as .env</DialogTitle>
          <DialogDescription>
            Every variable of one environment as <code>KEY=value</code> lines.
            Copy it, or edit and review the changes before they are saved.
            Opening it reveals each value, and every reveal is recorded in the
            audit log.
          </DialogDescription>
        </DialogHeader>

        {environments.length > 1 && (
          <div className="flex flex-wrap items-center gap-2">
            <Label htmlFor="raw-env-environment" className="text-sm">
              Environment
            </Label>
            <Select
              value={environment ? String(environment.id) : undefined}
              disabled={reviewing || apply.isPending}
              onValueChange={(value) => {
                setDraft(null)
                setChosenEnvironmentId(Number(value))
              }}
            >
              <SelectTrigger
                id="raw-env-environment"
                className="w-full sm:w-[220px]"
              >
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {environments.map((env) => (
                  <SelectItem key={env.id} value={String(env.id)}>
                    {env.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        )}

        {reviewing ? (
          <div className="space-y-2">
            <p className="text-sm text-muted-foreground">
              {applicable.length} change{applicable.length !== 1 ? 's' : ''} to{' '}
              {environment?.name}. Running deployments keep their current values
              until they are redeployed.
            </p>
            <ul className="max-h-[50vh] overflow-y-auto rounded-md border px-3">
              {changes.map((change) => (
                <ChangeRow
                  key={`${change.type}:${rawEnvChangeKey(change)}`}
                  change={change}
                  environmentName={environment?.name ?? 'this environment'}
                />
              ))}
            </ul>
          </div>
        ) : revealForbidden ? (
          <div role="alert" className="space-y-2 rounded-md border p-4 text-sm">
            <p className="font-medium">
              Your role can&apos;t read the values of {environment?.name}
            </p>
            <p className="text-muted-foreground">
              You can see which variables exist, but showing them as text needs
              the <code>environments:write</code> or <code>secrets:read</code>{' '}
              permission on this project. Ask a project or instance admin to
              change your role. Nothing was revealed or changed.
            </p>
          </div>
        ) : reveal.isError ? (
          <div role="alert" className="space-y-3 rounded-md border p-4">
            <p className="text-sm">
              Could not load the values of {environment?.name}:{' '}
              {getErrorMessage(reveal.error, 'request failed')}. Nothing was
              changed.
            </p>
            <Button variant="outline" onClick={() => void reveal.refetch()}>
              Retry
            </Button>
          </div>
        ) : reveal.isPending ? (
          <div role="status" aria-label="Loading values" className="space-y-2">
            {Array.from({ length: Math.min(revealable.length || 3, 8) }).map(
              (_, index) => (
                <Skeleton key={index} className="h-5 w-full" />
              )
            )}
          </div>
        ) : (
          <div className="space-y-2">
            <Textarea
              aria-label={`Environment variables for ${environment?.name}`}
              aria-invalid={parsed.errors.length > 0}
              value={text}
              onChange={(event) =>
                setDraft({ scope: draftScope, text: event.target.value })
              }
              spellCheck={false}
              placeholder={'DATABASE_URL=postgres://…\nAPI_KEY=…'}
              className="min-h-[320px] font-mono text-xs"
            />
            {parsed.errors.length > 0 && (
              <ul role="alert" className="space-y-1 text-xs text-destructive">
                {parsed.errors.slice(0, 5).map((error) => (
                  <li key={`${error.line}:${error.message}`}>
                    Line {error.line}: {error.message}
                  </li>
                ))}
                {parsed.errors.length > 5 && (
                  <li>{parsed.errors.length - 5} more problems</li>
                )}
              </ul>
            )}
            {secrets.length > 0 && (
              <p className="text-xs text-muted-foreground">
                {secrets.length} write-only secret
                {secrets.length !== 1 ? 's are' : ' is'} not shown and stay
                {secrets.length !== 1 ? '' : 's'} unchanged:{' '}
                <span className="font-mono">
                  {secrets.map(({ variable }) => variable.key).join(', ')}
                </span>
                . Add a line with a secret&apos;s name to replace its value.
              </p>
            )}
            <p className="text-xs text-muted-foreground">
              Variables provided by linked services are not included. A new line
              is added to {environment?.name} only; changing a value that is
              shared with other environments changes it there too, and the
              review step says where.
            </p>
          </div>
        )}

        <DialogFooter className="gap-2 sm:justify-between">
          <CopyButton
            value={text}
            label="Copy as .env"
            disabled={!reveal.data || reviewing}
            className="h-9 rounded-md border px-3"
          >
            Copy as .env
          </CopyButton>
          <div className="flex flex-col-reverse gap-2 sm:flex-row">
            {reviewing ? (
              <>
                <Button
                  variant="outline"
                  disabled={apply.isPending}
                  onClick={() => setReviewing(false)}
                >
                  Back to editor
                </Button>
                <Button
                  disabled={applicable.length === 0 || apply.isPending}
                  onClick={() =>
                    removedKeys.length > 0
                      ? setConfirmingRemovals(true)
                      : runApply()
                  }
                >
                  {apply.isPending && (
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  )}
                  Apply {applicable.length} change
                  {applicable.length !== 1 ? 's' : ''}
                </Button>
              </>
            ) : (
              <>
                <Button variant="outline" onClick={close}>
                  {reveal.isError ? 'Close' : 'Cancel'}
                </Button>
                {!reveal.isError && (
                  <Button
                    disabled={!canReview}
                    onClick={() => setReviewing(true)}
                  >
                    {changes.length === 0
                      ? 'No changes'
                      : `Review ${changes.length} change${changes.length !== 1 ? 's' : ''}`}
                  </Button>
                )}
              </>
            )}
          </div>
        </DialogFooter>
      </DialogContent>
      <ConfirmDeleteDialog
        open={confirmingRemovals}
        onOpenChange={setConfirmingRemovals}
        verb="Remove"
        resourceType={
          removedKeys.length === 1
            ? 'environment variable'
            : 'environment variables'
        }
        resourceName={summarizeKeys(removedKeys)}
        consequences={
          <RemovalConsequences
            removals={removals}
            environmentName={environment?.name ?? 'this environment'}
          />
        }
        isPending={apply.isPending}
        onConfirm={runApply}
      />
    </Dialog>
  )
}

function RemovalConsequences({
  removals,
  environmentName,
}: {
  removals: { deleted: string[]; unbound: string[] }
  environmentName: string
}) {
  return (
    <span className="block space-y-1">
      {removals.deleted.length > 0 && (
        <span className="block">
          Deleted, as {environmentName} is their only environment:{' '}
          <span className="font-mono">{removals.deleted.join(', ')}</span>.
        </span>
      )}
      {removals.unbound.length > 0 && (
        <span className="block">
          Removed from {environmentName} only; other environments keep them:{' '}
          <span className="font-mono">{removals.unbound.join(', ')}</span>.
        </span>
      )}
      <span className="block">
        Running deployments keep their values until they are redeployed.
      </span>
    </span>
  )
}
