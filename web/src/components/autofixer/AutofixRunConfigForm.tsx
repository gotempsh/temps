import { listAiProvidersOptions } from '@/api/client/@tanstack/react-query.gen'
import { startAnalysis } from '@/api/client/sdk.gen'
import type { AutofixRunConfig, ProjectResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
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
import { useQuery } from '@tanstack/react-query'
import { AlertTriangle, ArrowRight, Loader2, Wand2 } from 'lucide-react'
import { useMemo, useState } from 'react'
import { Link } from 'react-router'

// Sentinel for the "use provider default" model option — Radix Select
// rejects empty-string item values.
const DEFAULT_MODEL = '__default__'

interface AutofixRunConfigFormProps {
  project: ProjectResponse
  errorGroupId: number
  /** Prefill from a previous run's stored config (retry / start-over). */
  initialConfig?: AutofixRunConfig | null
  /** Prefill for the notes field (rarely used — retries start empty). */
  initialNotes?: string
  notesPlaceholder?: string
  submitLabel?: string
  onStarted: (runId: number) => void
  onCancel?: () => void
}

/**
 * Pre-run configuration for an AI autofix run: harness (provider), model,
 * max turns, branch to clone, and free-text notes for the model. Used both
 * on the initial "Fix with AI" screen and in the retry/start-over dialog,
 * prefilled from the previous run's stored config in the latter case.
 *
 * When no provider has a saved credential yet, renders the setup path
 * instead of a form that could only fail.
 */
export function AutofixRunConfigForm({
  project,
  errorGroupId,
  initialConfig,
  initialNotes = '',
  notesPlaceholder = 'Anything the model should know — where to look, constraints, suspected cause...',
  submitLabel = 'Start Analysis',
  onStarted,
  onCancel,
}: AutofixRunConfigFormProps) {
  const {
    data: catalog,
    isPending,
    isError,
  } = useQuery(listAiProvidersOptions())

  const [provider, setProvider] = useState<string | null>(
    initialConfig?.provider ?? null
  )
  const [model, setModel] = useState<string>(
    initialConfig?.model ?? DEFAULT_MODEL
  )
  const [maxTurns, setMaxTurns] = useState<string>(
    initialConfig?.max_turns != null ? String(initialConfig.max_turns) : ''
  )
  const [branch, setBranch] = useState<string>(initialConfig?.branch ?? '')
  const [notes, setNotes] = useState<string>(initialNotes)
  const [isStarting, setIsStarting] = useState(false)
  const [startError, setStartError] = useState<string | null>(null)

  const selectedProviderId =
    provider ?? catalog?.default_provider ?? 'claude_cli'
  const selectedProvider = useMemo(
    () => catalog?.providers.find((p) => p.id === selectedProviderId),
    [catalog, selectedProviderId]
  )
  const configuredProviders =
    catalog?.providers.filter((p) => p.credential_saved) ?? []

  if (isPending) {
    return (
      <div className="space-y-3">
        <Skeleton className="h-9 w-full" />
        <Skeleton className="h-9 w-full" />
        <Skeleton className="h-24 w-full" />
      </div>
    )
  }

  if (isError || !catalog) {
    return (
      <div className="flex items-start gap-2 rounded-md border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive">
        <AlertTriangle className="h-4 w-4 mt-0.5 shrink-0" />
        Failed to load the AI provider catalog. Refresh the page to retry.
      </div>
    )
  }

  // No usable provider yet — send the user to credential setup instead of
  // letting them start a run that can only fail with "Not logged in".
  if (configuredProviders.length === 0) {
    return (
      <div className="space-y-4">
        <div className="flex items-start gap-2 rounded-md border border-amber-500/30 bg-amber-500/5 p-3 text-sm">
          <AlertTriangle className="h-4 w-4 mt-0.5 shrink-0 text-amber-500" />
          <div>
            <p className="font-medium">No AI provider configured yet</p>
            <p className="text-muted-foreground mt-1">
              Autofix runs Claude Code, Codex, or OpenCode against your
              repository. Connect a subscription or API key for at least one
              provider first — then come back here to start the analysis.
            </p>
          </div>
        </div>
        <div className="space-y-2">
          {catalog.providers.map((p) => (
            <div
              key={p.id}
              className="flex items-center justify-between gap-3 rounded-md border p-3"
            >
              <div className="min-w-0">
                <p className="text-sm font-medium">{p.name}</p>
                <p className="text-xs text-muted-foreground truncate">
                  {p.auth_flavors.map((f) => f.label).join(' · ')}
                </p>
              </div>
              <Button asChild variant="outline" size="sm" className="shrink-0">
                <Link to={`/agent-sandbox/providers/${p.id}`}>
                  Configure
                  <ArrowRight className="h-3.5 w-3.5 ml-1.5" />
                </Link>
              </Button>
            </div>
          ))}
        </div>
      </div>
    )
  }

  const effectiveProvider = selectedProvider?.credential_saved
    ? selectedProviderId
    : configuredProviders[0].id
  const effectiveEntry = catalog.providers.find(
    (p) => p.id === effectiveProvider
  )
  const models = effectiveEntry?.models ?? []
  const supportsMaxTurns = effectiveEntry?.supports_max_turns ?? false
  const turnsDefaults = effectiveEntry
    ? `${effectiveEntry.max_turns_analysis ?? 10} analysis / ${effectiveEntry.max_turns_fix ?? 20} fix`
    : '10 analysis / 20 fix'

  const onSubmit = async () => {
    const turns = maxTurns.trim() === '' ? undefined : Number(maxTurns)
    if (
      turns !== undefined &&
      (!Number.isInteger(turns) || turns < 1 || turns > 200)
    ) {
      setStartError('Max turns must be a whole number between 1 and 200')
      return
    }
    setStartError(null)
    setIsStarting(true)
    try {
      const { data } = await startAnalysis({
        path: { project_id: project.id },
        body: {
          error_group_id: errorGroupId,
          user_context: notes.trim() || undefined,
          provider: effectiveProvider,
          model: model === DEFAULT_MODEL ? undefined : model,
          max_turns: turns,
          branch: branch.trim() || undefined,
        },
        throwOnError: true,
      })
      onStarted(data.id)
    } catch (e) {
      setStartError(e instanceof Error ? e.message : 'Failed to start analysis')
      setIsStarting(false)
    }
  }

  return (
    <div className="space-y-4">
      <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
        <div className="space-y-1.5">
          <Label>Harness</Label>
          <Select
            value={effectiveProvider}
            onValueChange={(v) => {
              if (!v) return
              setProvider(v)
              setModel(DEFAULT_MODEL)
            }}
          >
            <SelectTrigger>
              <SelectValue placeholder="Select a provider" />
            </SelectTrigger>
            <SelectContent>
              {catalog.providers.map((p) => (
                <SelectItem
                  key={p.id}
                  value={p.id}
                  disabled={!p.credential_saved}
                >
                  {p.name}
                  {!p.credential_saved && ' (not configured)'}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        {models.length > 0 && (
          <div className="space-y-1.5">
            <Label>Model</Label>
            <Select value={model} onValueChange={(v) => v && setModel(v)}>
              <SelectTrigger>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value={DEFAULT_MODEL}>
                  {effectiveEntry?.default_model
                    ? `Default (${effectiveEntry.default_model})`
                    : 'Provider default'}
                </SelectItem>
                {models.map((m) => (
                  <SelectItem key={m} value={m}>
                    {m}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        )}
        <div className="space-y-1.5">
          <Label htmlFor="autofix-max-turns">Max turns</Label>
          <Input
            id="autofix-max-turns"
            type="number"
            min={1}
            max={200}
            placeholder={turnsDefaults}
            value={maxTurns}
            onChange={(e) => setMaxTurns(e.target.value)}
          />
          <p className="text-xs text-muted-foreground">
            {supportsMaxTurns
              ? 'Caps every phase of this run. Empty = provider defaults.'
              : `Not enforced for ${effectiveEntry?.name ?? 'this provider'} — its CLI has no turn limit and runs to completion.`}
          </p>
        </div>
        <div className="space-y-1.5">
          <Label htmlFor="autofix-branch">Branch</Label>
          <Input
            id="autofix-branch"
            placeholder={project.main_branch || 'main'}
            value={branch}
            onChange={(e) => setBranch(e.target.value)}
          />
          <p className="text-xs text-muted-foreground">
            Repository: {project.repo_owner}/{project.repo_name}. Empty = main
            branch.
          </p>
        </div>
      </div>
      <div className="space-y-1.5">
        <Label htmlFor="autofix-notes">Notes for the model</Label>
        <Textarea
          id="autofix-notes"
          rows={3}
          placeholder={notesPlaceholder}
          value={notes}
          onChange={(e) => setNotes(e.target.value)}
        />
      </div>
      {startError && (
        <div className="flex items-start gap-2 rounded-md border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive">
          <AlertTriangle className="h-4 w-4 mt-0.5 shrink-0" />
          {startError}
        </div>
      )}
      <div className="flex justify-end gap-2">
        {onCancel && (
          <Button variant="ghost" onClick={onCancel} disabled={isStarting}>
            Cancel
          </Button>
        )}
        <Button onClick={onSubmit} disabled={isStarting}>
          {isStarting ? (
            <Loader2 className="h-4 w-4 mr-2 animate-spin" />
          ) : (
            <Wand2 className="h-4 w-4 mr-2" />
          )}
          {submitLabel}
        </Button>
      </div>
    </div>
  )
}
