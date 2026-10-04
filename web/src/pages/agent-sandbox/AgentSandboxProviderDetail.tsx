// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Link, useParams, useSearchParams } from 'react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useEffect, useRef, useState, type ReactNode } from 'react'
import { toast } from 'sonner'
import {
  AlertTriangle,
  ArrowLeft,
  ChevronRight,
  Loader2,
  RefreshCw,
  Save,
} from 'lucide-react'

import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { usePageTitle } from '@/hooks/usePageTitle'
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
import {
  activateAiProvider,
  updateAiProvider,
  type ProviderCatalogDto,
  type ProviderCatalogResponse,
} from '@/api/client'
import { AiHarnessLogo } from '@/components/ui/ai-harness-logo'
import { harnessSetupHref, workspaceReturnTo } from './harness-onboarding'
import { refreshAiProviderModelsMutation } from '@/api/client/@tanstack/react-query.gen'
import { problemDetail } from '@/lib/api-problem'
import { HarnessConnectionForm } from './HarnessConnectionForm'
import { HarnessPreflightChecks } from './HarnessPreflightChecks'
import { aiProviderCatalogQueryOptions } from '@/lib/ai-provider-catalog-query'
import {
  isSavedProviderModelUnavailable,
  mergeProviderModelRefresh,
} from './provider-model-catalog'

export function AgentSandboxProviderDetail() {
  const { id } = useParams<{ id: string }>()
  const [params] = useSearchParams()
  const returnTo = workspaceReturnTo(params.get('returnTo'))
  usePageTitle(id ? `Harness · ${id}` : 'Harness')
  const { data, isPending, isError } = useQuery({
    ...aiProviderCatalogQueryOptions,
    staleTime: 60 * 1000,
  })

  if (isPending) {
    return (
      <div className="space-y-4" aria-label="Loading harness">
        <Skeleton className="h-8 w-48" />
        <Skeleton className="h-48 w-full" />
      </div>
    )
  }

  if (isError || !data) {
    return (
      <Card>
        <CardContent className="py-8 text-sm text-destructive">
          Failed to load the harness catalog.
        </CardContent>
      </Card>
    )
  }

  const provider = data.providers.find((p) => p.id === id)
  if (!provider) {
    return (
      <Card>
        <CardContent className="py-8 space-y-3">
          <p className="text-sm">
            Harness <code className="font-mono">{id}</code> is not in the
            catalog.
          </p>
          <Button asChild variant="outline" size="sm">
            <Link to={harnessSetupHref(null, returnTo)}>
              <ArrowLeft className="h-3.5 w-3.5 mr-1.5" />
              Back to harnesses
            </Link>
          </Button>
        </CardContent>
      </Card>
    )
  }

  return (
    <div className="space-y-4">
      <Link
        to={harnessSetupHref(null, returnTo)}
        className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
      >
        <ArrowLeft className="h-3.5 w-3.5" />
        All harnesses
      </Link>

      <ProviderEditor
        key={provider.id}
        provider={provider}
        isActive={provider.id === data.default_provider}
        returnTo={returnTo}
      />
    </div>
  )
}

// ── Editor ──────────────────────────────────────────────────────────────────
// Connecting is the only thing most people come here for, so it is the only
// thing shown by default. Everything that tunes a working connection (default
// model, diagnostics, instance default, autofix limits) sits behind Advanced.

interface ProviderEditorProps {
  provider: ProviderCatalogDto
  isActive: boolean
  returnTo?: string
  embedded?: boolean
  onVerificationPending?: (pending: boolean) => void
}

export function ProviderEditor({
  provider,
  isActive,
  returnTo = '/ai-first',
  embedded = false,
  onVerificationPending,
}: ProviderEditorProps) {
  if (embedded) {
    return (
      <div className="space-y-4">
        <p className="text-sm text-muted-foreground">
          Connect once. Reuse this account across your workspaces.
        </p>
        <HarnessConnectionForm
          provider={provider}
          onVerificationPending={onVerificationPending}
        />
      </div>
    )
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex items-center gap-3">
          <AiHarnessLogo providerId={provider.id} size={32} />
          <h2 className="text-lg font-semibold">{provider.name}</h2>
        </div>
        <Button asChild variant="outline" size="sm">
          <Link to={returnTo}>Back to workspace</Link>
        </Button>
      </div>

      <Card className="shadow-none">
        <CardHeader className="pb-3">
          <CardTitle className="text-base">Sign in</CardTitle>
        </CardHeader>
        <CardContent>
          <HarnessConnectionForm
            provider={provider}
            onVerificationPending={onVerificationPending}
          />
        </CardContent>
      </Card>

      <details className="group rounded-lg border">
        <summary className="flex cursor-pointer list-none items-center gap-2 rounded-lg p-4 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring [&::-webkit-details-marker]:hidden">
          <ChevronRight
            className="size-4 shrink-0 transition-transform group-open:rotate-90"
            aria-hidden="true"
          />
          <span className="font-medium">Advanced</span>
          <span className="truncate text-muted-foreground">
            Default model, diagnostics, instance default, autofix limits
          </span>
        </summary>
        <div className="space-y-6 border-t p-4">
          <DefaultModelSetting provider={provider} />
          <HarnessPreflightChecks
            key={JSON.stringify(provider)}
            providerId={provider.id}
            model={provider.default_model}
            credentialSaved={provider.credential_saved}
          />
          <InstanceDefaultSetting provider={provider} isActive={isActive} />
          <TurnLimitsSetting provider={provider} />
        </div>
      </details>
    </div>
  )
}

function SettingHeading({
  title,
  description,
  children,
}: {
  title: string
  description: string
  children?: ReactNode
}) {
  return (
    <div className="flex flex-wrap items-start justify-between gap-3">
      <div className="min-w-0">
        <h3 className="text-sm font-medium">{title}</h3>
        <p className="mt-1 text-xs text-muted-foreground">{description}</p>
      </div>
      {children}
    </div>
  )
}

// ── Default model ───────────────────────────────────────────────────────────
// Workspaces pick a model per thread; this default applies to server-side
// workflows (autofix) and to the smoke test.

function DefaultModelSetting({ provider }: { provider: ProviderCatalogDto }) {
  const queryClient = useQueryClient()
  const initialModel = provider.default_model ?? ''
  const [serverModel, setServerModel] = useState(initialModel)
  const [modelDraft, setModelDraft] = useState(initialModel)
  const [customMode, setCustomMode] = useState(
    provider.models.length === 0 ||
      (initialModel !== '' && !provider.models.includes(initialModel))
  )
  const [savingModel, setSavingModel] = useState(false)
  const refreshModelsMutation = useMutation(refreshAiProviderModelsMutation())

  useEffect(() => {
    const fresh = provider.default_model ?? ''
    if (fresh !== serverModel) {
      const reset = window.setTimeout(() => {
        setServerModel(fresh)
        setModelDraft(fresh)
        setCustomMode(
          provider.models.length === 0 ||
            (fresh !== '' && !provider.models.includes(fresh))
        )
      }, 0)
      return () => window.clearTimeout(reset)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [provider.default_model])

  const savedModelUnavailable = isSavedProviderModelUnavailable({
    savedModel: serverModel,
    availableModels: provider.models,
    source: provider.model_source,
  })

  const handleRefreshModels = async () => {
    try {
      const refreshed = await refreshModelsMutation.mutateAsync({
        path: { provider_id: provider.id },
      })
      queryClient.setQueryData<ProviderCatalogResponse>(
        aiProviderCatalogQueryOptions.queryKey,
        (catalog) =>
          catalog ? mergeProviderModelRefresh(catalog, refreshed) : catalog
      )
      const freshModel = provider.default_model ?? ''
      const refreshedModelIds = refreshed.runtime_models.map(
        (model) => model.id
      )
      setCustomMode(
        refreshedModelIds.length === 0 ||
          (freshModel !== '' && !refreshedModelIds.includes(freshModel))
      )
      void queryClient.invalidateQueries({
        queryKey: aiProviderCatalogQueryOptions.queryKey,
      })

      if (
        refreshed.model_source === 'live' ||
        refreshed.model_source === 'cache'
      ) {
        toast.success(`${provider.name} models refreshed`, {
          description: `${refreshed.runtime_models.length} models reported by ${
            provider.workspace_ready
              ? 'the saved workspace credential'
              : 'the authenticated host CLI'
          }.`,
        })
      } else {
        toast.warning(`Could not refresh ${provider.name} models`, {
          description:
            'Temps kept the last known model list because live discovery did not complete.',
        })
      }
    } catch (cause) {
      toast.error(`Could not refresh ${provider.name} models`, {
        description: problemDetail(
          cause,
          'The provider did not return a model catalog. Check its credential and try again.'
        ),
      })
    }
  }

  const persistModel = async (next: string) => {
    if (next === serverModel) return
    setSavingModel(true)
    try {
      await updateAiProvider({
        path: { provider_id: provider.id },
        body: { default_model: next },
        throwOnError: true,
      })
      setServerModel(next)
      toast.success(
        next === ''
          ? `${provider.name} will use its default model`
          : `${provider.name} model set to ${next}`
      )
      await queryClient.invalidateQueries({
        queryKey: aiProviderCatalogQueryOptions.queryKey,
      })
    } catch (e) {
      toast.error(`Failed to save ${provider.name} model`, {
        description: problemDetail(
          e,
          'The request failed. Check your connection and permissions, then retry.'
        ),
      })
      setModelDraft(serverModel)
    } finally {
      setSavingModel(false)
    }
  }

  // Debounced custom-model save.
  const debounceTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  useEffect(() => {
    if (!customMode) return
    if (modelDraft === serverModel) return
    if (debounceTimerRef.current) clearTimeout(debounceTimerRef.current)
    debounceTimerRef.current = setTimeout(() => {
      void persistModel(modelDraft.trim())
    }, 600)
    return () => {
      if (debounceTimerRef.current) clearTimeout(debounceTimerRef.current)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [modelDraft, customMode, serverModel])

  return (
    <section className="space-y-3">
      <SettingHeading
        title="Default model"
        description={
          provider.models_refreshed_at
            ? `Used by autofix and the smoke test. Workspaces pick a model per chat. Last refreshed ${new Date(provider.models_refreshed_at).toLocaleString()}.`
            : 'Used by autofix and the smoke test. Workspaces pick a model per chat.'
        }
      >
        <div className="flex items-center gap-2">
          {savingModel && (
            <span className="inline-flex items-center gap-1 text-xs text-muted-foreground">
              <Loader2 className="h-3 w-3 animate-spin" />
              Saving…
            </span>
          )}
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => void handleRefreshModels()}
            disabled={refreshModelsMutation.isPending}
          >
            <RefreshCw
              className={`mr-1.5 h-3.5 w-3.5 ${refreshModelsMutation.isPending ? 'animate-spin' : ''}`}
            />
            {refreshModelsMutation.isPending ? 'Refreshing…' : 'Refresh models'}
          </Button>
        </div>
      </SettingHeading>
      {savedModelUnavailable && (
        <div
          role="alert"
          className="flex items-start gap-2 rounded-md border border-amber-500/30 bg-amber-500/5 px-3 py-2 text-xs text-amber-800 dark:text-amber-200"
        >
          <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
          <span>
            The saved model <code>{serverModel}</code> was not reported by the
            refreshed CLI. Choose an available model or use the provider default
            before starting another turn.
          </span>
        </div>
      )}
      {provider.models.length > 0 && !customMode ? (
        <Select
          value={modelDraft === '' ? '_default' : modelDraft}
          onValueChange={(v) => {
            if (v === '_custom') {
              setCustomMode(true)
              return
            }
            const next = v === '_default' ? '' : v
            setModelDraft(next)
            void persistModel(next)
          }}
        >
          <SelectTrigger
            id={`model-${provider.id}`}
            aria-label="Default model"
            className="w-full sm:w-[320px]"
          >
            <SelectValue placeholder="Use provider default" />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="_default">Use provider default</SelectItem>
            {provider.models.map((model) => (
              <SelectItem key={model} value={model}>
                {model}
              </SelectItem>
            ))}
            <SelectItem value="_custom">Custom model…</SelectItem>
          </SelectContent>
        </Select>
      ) : (
        <div className="flex gap-2 sm:w-[420px]">
          <Input
            id={`model-${provider.id}`}
            aria-label="Default model"
            placeholder={
              provider.id === 'opencode'
                ? 'e.g. anthropic/claude-sonnet-4-6'
                : 'Leave blank for the provider default'
            }
            value={modelDraft}
            onChange={(e) => setModelDraft(e.target.value)}
          />
          {provider.models.length > 0 && (
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => {
                setCustomMode(false)
                setModelDraft(serverModel)
              }}
            >
              Cancel
            </Button>
          )}
        </div>
      )}
    </section>
  )
}

// ── Instance default ────────────────────────────────────────────────────────

function InstanceDefaultSetting({
  provider,
  isActive,
}: {
  provider: ProviderCatalogDto
  isActive: boolean
}) {
  const queryClient = useQueryClient()
  const [activating, setActivating] = useState(false)

  const handleActivate = async () => {
    setActivating(true)
    try {
      await activateAiProvider({
        path: { provider_id: provider.id },
        throwOnError: true,
      })
      toast.success(`${provider.name} is now the instance default`)
      await Promise.all([
        queryClient.invalidateQueries({
          queryKey: aiProviderCatalogQueryOptions.queryKey,
        }),
        queryClient.invalidateQueries({ queryKey: ['platform-settings'] }),
      ])
    } catch (e) {
      toast.error(`Failed to make ${provider.name} the instance default`, {
        description: problemDetail(
          e,
          'The request failed. Check your connection and permissions, then retry.'
        ),
      })
    } finally {
      setActivating(false)
    }
  }

  return (
    <section>
      <SettingHeading
        title="Instance default"
        description="Server-side workflows use this harness. It does not change the harness selected in an existing workspace thread."
      >
        <Button
          variant="outline"
          size="sm"
          onClick={() => void handleActivate()}
          disabled={isActive || activating}
        >
          {activating
            ? 'Saving…'
            : isActive
              ? 'Instance default'
              : 'Use as instance default'}
        </Button>
      </SettingHeading>
    </section>
  )
}

// ── Autofix turn limits ─────────────────────────────────────────────────────
// Per-provider defaults for the autofixer's turn caps. Per-run overrides in
// the "Fix with AI" dialog take precedence; blank fields fall back to the
// built-in defaults (10 analysis / 20 fix / 10 feedback).

const TURN_FIELDS = [
  {
    key: 'max_turns_analysis' as const,
    label: 'Analysis',
    builtin: 10,
  },
  {
    key: 'max_turns_fix' as const,
    label: 'Fix',
    builtin: 20,
  },
  {
    key: 'max_turns_feedback' as const,
    label: 'Feedback',
    builtin: 10,
  },
]

function TurnLimitsSetting({ provider }: { provider: ProviderCatalogDto }) {
  const queryClient = useQueryClient()
  const [drafts, setDrafts] = useState<Record<string, string>>({
    max_turns_analysis: provider.max_turns_analysis?.toString() ?? '',
    max_turns_fix: provider.max_turns_fix?.toString() ?? '',
    max_turns_feedback: provider.max_turns_feedback?.toString() ?? '',
  })
  const [savingTurns, setSavingTurns] = useState(false)

  const dirty = TURN_FIELDS.some(
    (f) => drafts[f.key] !== (provider[f.key]?.toString() ?? '')
  )

  const handleSaveTurns = async () => {
    const body: Record<string, number> = {}
    for (const f of TURN_FIELDS) {
      const raw = drafts[f.key].trim()
      // Blank = clear back to built-in default (API: 0 clears, omitted keeps)
      const value = raw === '' ? 0 : Number(raw)
      if (
        raw !== '' &&
        (!Number.isInteger(value) || value < 1 || value > 200)
      ) {
        toast.error(`${f.label} turns must be a whole number between 1 and 200`)
        return
      }
      body[f.key] = value
    }
    setSavingTurns(true)
    try {
      await updateAiProvider({
        path: { provider_id: provider.id },
        body,
        throwOnError: true,
      })
      toast.success(`${provider.name} turn limits saved`)
      await queryClient.invalidateQueries({
        queryKey: aiProviderCatalogQueryOptions.queryKey,
      })
    } catch (e) {
      toast.error(`Failed to save ${provider.name} turn limits`, {
        description: problemDetail(
          e,
          'The request failed. Check your connection and permissions, then retry.'
        ),
      })
    } finally {
      setSavingTurns(false)
    }
  }

  return (
    <section className="space-y-3">
      <SettingHeading
        title="Autofix turn limits"
        description={
          provider.supports_max_turns
            ? 'Max agent turns per autofix phase. "Fix with AI" overrides take precedence. Blank uses the built-in default.'
            : `${provider.name}'s CLI has no turn-limit flag, so these values are stored but not enforced.`
        }
      >
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={handleSaveTurns}
          disabled={savingTurns || !dirty}
        >
          {savingTurns ? (
            <Loader2 className="h-3.5 w-3.5 animate-spin mr-1.5" />
          ) : (
            <Save className="h-3.5 w-3.5 mr-1.5" />
          )}
          Save limits
        </Button>
      </SettingHeading>
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
        {TURN_FIELDS.map((f) => (
          <div key={f.key} className="space-y-1.5">
            <Label htmlFor={`${f.key}-${provider.id}`}>{f.label}</Label>
            <Input
              id={`${f.key}-${provider.id}`}
              type="number"
              min={1}
              max={200}
              placeholder={`${f.builtin} (default)`}
              value={drafts[f.key]}
              onChange={(e) =>
                setDrafts((d) => ({ ...d, [f.key]: e.target.value }))
              }
            />
          </div>
        ))}
      </div>
    </section>
  )
}
