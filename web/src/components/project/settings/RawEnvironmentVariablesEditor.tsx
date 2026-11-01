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
  parseEnvText,
  planRawEnvChanges,
  rawEditorVariables,
  serializeEnvText,
  type RawEnvChange,
} from '@/lib/raw-env-editor'
import type { VariableChangeScope } from '@/lib/variable-change-redeploy'
import { getErrorMessage } from '@/utils/errorHandling'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Loader2 } from 'lucide-react'
import { useMemo, useState } from 'react'
import { toast } from 'sonner'

/** Parallel reveals; each one is a separate audited request. */
const REVEAL_CONCURRENCY = 4

async function mapWithConcurrency<T, R>(
  items: readonly T[],
  limit: number,
  run: (item: T) => Promise<R>
): Promise<R[]> {
  const results = new Array<R>(items.length)
  let next = 0
  const worker = async () => {
    while (next < items.length) {
      const index = next++
      results[index] = await run(items[index])
    }
  }
  await Promise.all(
    Array.from({ length: Math.min(limit, items.length) }, worker)
  )
  return results
}

function changeKey(change: RawEnvChange): string {
  return change.type === 'create' ? change.key : change.variable.key
}

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
        <span className="font-mono text-sm break-all">{changeKey(change)}</span>
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
  const revealQueryKey = [
    'raw-env-editor',
    projectId,
    environment?.id ?? null,
    revealable.map(({ variable }) => variable.id).join(','),
  ]

  const reveal = useQuery({
    queryKey: revealQueryKey,
    queryFn: async () => {
      const values = await mapWithConcurrency(
        revealable,
        REVEAL_CONCURRENCY,
        async ({ variable }) =>
          [
            variable.id,
            await getEnvVarValue(projectId, variable.key, variable.id),
          ] as const
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

  const baselineText = useMemo(
    () =>
      reveal.data
        ? serializeEnvText(
            revealable.map(({ variable }) => ({
              key: variable.key,
              value: reveal.data.get(variable.id) ?? '',
            }))
          )
        : '',
    [reveal.data, revealable]
  )
  const draftScope = `${environment?.id ?? 'none'}:${reveal.dataUpdatedAt}`
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

  const close = () => {
    setDraft(null)
    setReviewing(false)
    setChosenEnvironmentId(null)
    queryClient.removeQueries({ queryKey: ['raw-env-editor', projectId] })
    onOpenChange(false)
  }

  const apply = useMutation({
    mutationFn: async () => {
      if (!environment) return { applied: 0, failures: [], scope: [] }
      const failures: { key: string; message: string }[] = []
      const scope = new Set<number>([environment.id])
      let applied = 0
      // Additions and edits first, removals last: if something fails part
      // way, fewer values have been taken away.
      const ordered = [
        ...applicable.filter((c) => c.type === 'create' || c.type === 'update'),
        ...applicable.filter((c) => c.type === 'unbind' || c.type === 'delete'),
      ]
      for (const change of ordered) {
        try {
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
          applied++
        } catch (error) {
          failures.push({
            key: changeKey(change),
            message: getErrorMessage(error, 'Request failed'),
          })
        }
      }
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
      }
      close()
    },
  })

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
                  key={`${change.type}:${changeKey(change)}`}
                  change={change}
                  environmentName={environment?.name ?? 'this environment'}
                />
              ))}
            </ul>
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
                  onClick={() => apply.mutate()}
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
                  Cancel
                </Button>
                <Button
                  disabled={!canReview}
                  onClick={() => setReviewing(true)}
                >
                  {changes.length === 0
                    ? 'No changes'
                    : `Review ${changes.length} change${changes.length !== 1 ? 's' : ''}`}
                </Button>
              </>
            )}
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
