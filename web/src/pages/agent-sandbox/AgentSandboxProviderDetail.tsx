// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Link, useParams } from 'react-router'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useEffect, useRef, useState } from 'react'
import { toast } from 'sonner'
import {
  ArrowLeft,
  CheckCircle2,
  Loader2,
  Play,
  Save,
  XCircle,
} from 'lucide-react'

import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
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
import { Textarea } from '@/components/ui/textarea'

type CredentialFormat = 'api_key' | 'oauth_token' | 'config_file'

interface AuthFlavorDto {
  id: string
  label: string
  description: string
  format: CredentialFormat
  env_var: string | null
}

interface ProviderCatalogDto {
  id: string
  name: string
  install_command: string
  auth_command: string
  auth_flavors: AuthFlavorDto[]
  credential_saved: boolean
  current_auth_type: string | null
  models: string[]
  default_model: string | null
  max_turns_analysis: number | null
  max_turns_fix: number | null
  max_turns_feedback: number | null
  supports_max_turns: boolean
}

interface ProviderCatalogResponse {
  default_provider: string
  providers: ProviderCatalogDto[]
}

async function fetchCatalog(): Promise<ProviderCatalogResponse> {
  const r = await fetch('/api/settings/ai-providers')
  if (!r.ok) throw new Error(`Failed to load AI provider catalog (${r.status})`)
  return r.json()
}

export function AgentSandboxProviderDetail() {
  const { id } = useParams<{ id: string }>()
  usePageTitle(id ? `Provider · ${id}` : 'AI Provider')
  const { data, isPending, isError } = useQuery({
    queryKey: ['ai-provider-catalog'],
    queryFn: fetchCatalog,
    staleTime: 60 * 1000,
  })

  if (isPending) {
    return (
      <div className="flex justify-center py-12">
        <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
      </div>
    )
  }

  if (isError || !data) {
    return (
      <Card>
        <CardContent className="py-8 text-sm text-destructive">
          Failed to load AI provider catalog.
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
            Provider <code className="font-mono">{id}</code> is not in the
            catalog.
          </p>
          <Button asChild variant="outline" size="sm">
            <Link to="/agent-sandbox/providers">
              <ArrowLeft className="h-3.5 w-3.5 mr-1.5" />
              Back to providers
            </Link>
          </Button>
        </CardContent>
      </Card>
    )
  }

  const isActive = provider.id === data.default_provider

  return (
    <div className="space-y-4">
      <Link
        to="/agent-sandbox/providers"
        className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
      >
        <ArrowLeft className="h-3.5 w-3.5" />
        All providers
      </Link>

      <ProviderEditor provider={provider} isActive={isActive} />
    </div>
  )
}

// ── Editor ──────────────────────────────────────────────────────────────────
// This is the same form that used to live inline in AiProvidersCard.tsx, just
// rendered full-bleed instead of stacked next to two siblings. Behavior is
// unchanged — the endpoints, flavors, and model handling all work the same.

interface ProviderEditorProps {
  provider: ProviderCatalogDto
  isActive: boolean
}

function ProviderEditor({ provider, isActive }: ProviderEditorProps) {
  const queryClient = useQueryClient()
  const defaultFlavor =
    provider.auth_flavors.find((f) => f.id === provider.current_auth_type) ??
    provider.auth_flavors[0]

  const [selectedFlavorId, setSelectedFlavorId] = useState(defaultFlavor.id)
  const [credential, setCredential] = useState('')
  const [saving, setSaving] = useState(false)
  const [activating, setActivating] = useState(false)
  const [testing, setTesting] = useState(false)
  const [testResult, setTestResult] = useState<{
    passed: boolean
    environment: string
    cli_version: string | null
    auth_info: string | null
    setup_hint: string | null
  } | null>(null)

  const initialModel = provider.default_model ?? ''
  const [serverModel, setServerModel] = useState(initialModel)
  const [modelDraft, setModelDraft] = useState(initialModel)
  const [customMode, setCustomMode] = useState(
    provider.models.length === 0 ||
      (initialModel !== '' && !provider.models.includes(initialModel))
  )
  const [savingModel, setSavingModel] = useState(false)

  useEffect(() => {
    const fresh = provider.default_model ?? ''
    if (fresh !== serverModel) {
      setServerModel(fresh)
      setModelDraft(fresh)
      setCustomMode(
        provider.models.length === 0 ||
          (fresh !== '' && !provider.models.includes(fresh))
      )
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [provider.default_model])

  const selectedFlavor =
    provider.auth_flavors.find((f) => f.id === selectedFlavorId) ??
    defaultFlavor

  const persistModel = async (next: string) => {
    if (next === serverModel) return
    setSavingModel(true)
    try {
      const res = await fetch(`/api/settings/ai-providers/${provider.id}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ default_model: next }),
      })
      if (!res.ok) {
        const detail = await res.text()
        toast.error(`Failed to save ${provider.name} model`, {
          description: detail.slice(0, 200),
        })
        setModelDraft(serverModel)
        setCustomMode(
          provider.models.length === 0 ||
            (serverModel !== '' && !provider.models.includes(serverModel))
        )
        return
      }
      setServerModel(next)
      toast.success(
        next === ''
          ? `${provider.name} will use its default model`
          : `${provider.name} model set to ${next}`
      )
      await queryClient.invalidateQueries({ queryKey: ['ai-provider-catalog'] })
    } catch (e) {
      toast.error(`Failed to save ${provider.name} model`, {
        description: e instanceof Error ? e.message : 'Network error',
      })
      setModelDraft(serverModel)
    } finally {
      setSavingModel(false)
    }
  }

  // Debounced custom-model save — same pattern as the old card.
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

  const handleActivate = async () => {
    setActivating(true)
    try {
      const res = await fetch(
        `/api/settings/ai-providers/${provider.id}/activate`,
        { method: 'POST' }
      )
      if (!res.ok) {
        const detail = await res.text()
        toast.error(`Failed to activate ${provider.name}`, {
          description: detail.slice(0, 200),
        })
        return
      }
      toast.success(`${provider.name} is now the active provider`)
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ['ai-provider-catalog'] }),
        queryClient.invalidateQueries({ queryKey: ['platform-settings'] }),
      ])
    } catch (e) {
      toast.error(`Failed to activate ${provider.name}`, {
        description: e instanceof Error ? e.message : 'Network error',
      })
    } finally {
      setActivating(false)
    }
  }

  const handleTest = async () => {
    setTesting(true)
    setTestResult(null)
    try {
      const res = await fetch(
        `/api/projects/0/agents/smoke-test?provider_id=${encodeURIComponent(provider.id)}`,
        { method: 'POST' }
      )
      if (!res.ok) {
        const detail = await res.text()
        toast.error('Smoke test failed', { description: detail.slice(0, 200) })
        return
      }
      const data = await res.json()
      setTestResult({
        passed: data.passed,
        environment: data.environment,
        cli_version: data.cli_version,
        auth_info: data.auth_info,
        setup_hint: data.setup_hint,
      })
      if (data.passed) {
        toast.success(`${provider.name} is ready`)
      } else {
        toast.error(`${provider.name} test failed`, {
          description: data.setup_hint ?? 'See card for details',
        })
      }
    } catch (e) {
      toast.error('Smoke test failed', {
        description: e instanceof Error ? e.message : 'Network error',
      })
    } finally {
      setTesting(false)
    }
  }

  const handleSave = async () => {
    if (!credential.trim()) return
    setSaving(true)
    try {
      const res = await fetch(
        `/api/settings/ai-providers/${provider.id}/credential`,
        {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({
            auth_type: selectedFlavor.id,
            credential: credential.trim(),
          }),
        }
      )
      if (!res.ok) {
        const detail = await res.text()
        toast.error(`Failed to save ${provider.name} credential`, {
          description: detail.slice(0, 200),
        })
        return
      }
      toast.success(`${provider.name} credential encrypted and saved`)
      setCredential('')
      await queryClient.invalidateQueries({ queryKey: ['ai-provider-catalog'] })
    } catch (e) {
      toast.error(`Failed to save ${provider.name} credential`, {
        description: e instanceof Error ? e.message : 'Network error',
      })
    } finally {
      setSaving(false)
    }
  }

  return (
    <div className="space-y-4">
      <Card>
        <CardHeader>
          <div className="flex items-start justify-between gap-3 flex-wrap">
            <div>
              <CardTitle className="flex items-center gap-2">
                {provider.name}
                {isActive && (
                  <span className="inline-flex items-center rounded-full border border-primary/30 bg-primary/10 px-2 py-0.5 text-xs font-medium text-primary">
                    Active
                  </span>
                )}
                {provider.credential_saved && !isActive && (
                  <span className="inline-flex items-center gap-1 text-xs text-green-500">
                    <CheckCircle2 className="h-3.5 w-3.5" />
                    Configured
                  </span>
                )}
              </CardTitle>
              <CardDescription className="mt-1 space-y-0.5">
                <span className="block">
                  Install:{' '}
                  <code className="bg-muted px-1 rounded">
                    {provider.install_command}
                  </code>
                </span>
                <span className="block">
                  Auth:{' '}
                  <code className="bg-muted px-1 rounded">
                    {provider.auth_command}
                  </code>
                </span>
              </CardDescription>
            </div>
            <div className="flex items-center gap-2 shrink-0">
              {provider.credential_saved && (
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={handleTest}
                  disabled={testing}
                >
                  {testing ? (
                    <Loader2 className="h-3.5 w-3.5 animate-spin mr-1.5" />
                  ) : (
                    <Play className="h-3.5 w-3.5 mr-1.5" />
                  )}
                  Test
                </Button>
              )}
              {provider.credential_saved && (
                <Button
                  type="button"
                  variant={isActive ? 'default' : 'outline'}
                  size="sm"
                  onClick={handleActivate}
                  disabled={isActive || activating}
                >
                  {activating ? (
                    <Loader2 className="h-3.5 w-3.5 animate-spin mr-1.5" />
                  ) : null}
                  {isActive ? 'Active' : 'Use this'}
                </Button>
              )}
            </div>
          </div>
        </CardHeader>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">Credential</CardTitle>
          <CardDescription>
            Encrypted with AES-256-GCM at rest and injected into each session.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          {provider.auth_flavors.length > 1 && (
            <div className="grid grid-cols-1 sm:grid-cols-2 gap-2">
              {provider.auth_flavors.map((flavor) => (
                <button
                  key={flavor.id}
                  type="button"
                  onClick={() => setSelectedFlavorId(flavor.id)}
                  className={`rounded-md border p-2.5 text-left transition-colors ${
                    selectedFlavorId === flavor.id
                      ? 'border-primary bg-primary/10'
                      : 'border-border hover:border-primary/50'
                  }`}
                >
                  <p className="text-xs font-medium">{flavor.label}</p>
                  <p className="text-[11px] text-muted-foreground mt-0.5">
                    {flavor.description}
                  </p>
                </button>
              ))}
            </div>
          )}

          <div className="space-y-2">
            <Label htmlFor={`cred-${provider.id}`}>
              {selectedFlavor.label} credential
              {selectedFlavor.env_var && (
                <span className="ml-2 text-xs font-normal text-muted-foreground">
                  → injected as {selectedFlavor.env_var}
                </span>
              )}
            </Label>
            <p className="text-xs text-muted-foreground">
              {selectedFlavor.description}
            </p>
            {selectedFlavor.format === 'config_file' ? (
              <Textarea
                id={`cred-${provider.id}`}
                placeholder={
                  provider.credential_saved
                    ? '••••••••••••• (saved — paste a new file body to replace)'
                    : 'Paste the full file contents here...'
                }
                value={credential}
                onChange={(e) => setCredential(e.target.value)}
                className="min-h-[140px] font-mono text-xs"
              />
            ) : (
              <Input
                id={`cred-${provider.id}`}
                type="password"
                placeholder={
                  provider.credential_saved
                    ? '••••••••••••• (saved — paste a new value to replace)'
                    : selectedFlavor.format === 'oauth_token'
                      ? 'Paste OAuth token...'
                      : 'Paste API key...'
                }
                value={credential}
                onChange={(e) => setCredential(e.target.value)}
              />
            )}
            <div className="flex justify-end">
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={handleSave}
                disabled={saving || !credential.trim()}
              >
                {saving ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin mr-1.5" />
                ) : (
                  <Save className="h-3.5 w-3.5 mr-1.5" />
                )}
                Save credential
              </Button>
            </div>
          </div>

          {testResult && (
            <div
              className={`rounded-md border p-3 text-xs space-y-1 ${
                testResult.passed
                  ? 'border-green-500/30 bg-green-500/5'
                  : 'border-red-500/30 bg-red-500/5'
              }`}
            >
              <div className="flex items-center gap-1.5 font-medium">
                {testResult.passed ? (
                  <CheckCircle2 className="h-3.5 w-3.5 text-green-500" />
                ) : (
                  <XCircle className="h-3.5 w-3.5 text-red-500" />
                )}
                {testResult.passed ? 'Connected' : 'Test failed'}
                <span className="text-muted-foreground font-normal">
                  ({testResult.environment})
                </span>
              </div>
              {testResult.cli_version && (
                <p className="text-muted-foreground">
                  Version:{' '}
                  <code className="bg-muted px-1 rounded">
                    {testResult.cli_version}
                  </code>
                </p>
              )}
              {testResult.auth_info && (
                <p className="text-muted-foreground">
                  Auth: {testResult.auth_info}
                </p>
              )}
              {testResult.setup_hint && (
                <p className="text-muted-foreground">{testResult.setup_hint}</p>
              )}
            </div>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <div className="flex items-center justify-between">
            <div>
              <CardTitle className="text-base">Default model</CardTitle>
              <CardDescription>
                {provider.id === 'opencode'
                  ? 'OpenCode resolves models via ~/.config/opencode/config.json or per-session flags. Setting a value here exports it to OPENCODE_MODEL.'
                  : 'Leave blank to let the CLI pick. Custom values are accepted — the catalog is just a convenience list.'}
              </CardDescription>
            </div>
            {savingModel && (
              <span className="inline-flex items-center gap-1 text-xs text-muted-foreground">
                <Loader2 className="h-3 w-3 animate-spin" />
                Saving…
              </span>
            )}
          </div>
        </CardHeader>
        <CardContent>
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
              <SelectTrigger id={`model-${provider.id}`}>
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
            <div className="flex gap-2">
              <Input
                id={`model-${provider.id}`}
                placeholder={
                  provider.id === 'claude_cli'
                    ? 'e.g. claude-sonnet-4-6'
                    : provider.id === 'codex_cli'
                      ? 'e.g. gpt-5-codex'
                      : provider.id === 'opencode'
                        ? 'e.g. anthropic/claude-sonnet-4-6'
                        : 'Model id'
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
        </CardContent>
      </Card>

      <TurnLimitsCard provider={provider} />
    </div>
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
    hint: 'Root-cause investigation',
  },
  {
    key: 'max_turns_fix' as const,
    label: 'Fix',
    builtin: 20,
    hint: 'Writing the fix and tests',
  },
  {
    key: 'max_turns_feedback' as const,
    label: 'Feedback',
    builtin: 10,
    hint: 'Follow-up conversation rounds',
  },
]

function TurnLimitsCard({ provider }: { provider: ProviderCatalogDto }) {
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
      const res = await fetch(`/api/settings/ai-providers/${provider.id}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      })
      if (!res.ok) {
        const detail = await res.text()
        toast.error(`Failed to save ${provider.name} turn limits`, {
          description: detail.slice(0, 200),
        })
        return
      }
      toast.success(`${provider.name} turn limits saved`)
      await queryClient.invalidateQueries({ queryKey: ['ai-provider-catalog'] })
    } catch (e) {
      toast.error(`Failed to save ${provider.name} turn limits`, {
        description: e instanceof Error ? e.message : 'Network error',
      })
    } finally {
      setSavingTurns(false)
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-base">Autofix turn limits</CardTitle>
        <CardDescription>
          {provider.supports_max_turns
            ? 'Default max agent turns per autofix phase when this provider runs. Per-run overrides in the "Fix with AI" dialog take precedence. Blank = built-in default.'
            : `${provider.name}'s CLI has no turn-limit flag, so these values are stored but not enforced — runs continue until the CLI finishes on its own.`}
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="grid grid-cols-1 sm:grid-cols-3 gap-4">
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
              <p className="text-xs text-muted-foreground">{f.hint}</p>
            </div>
          ))}
        </div>
        <div className="flex justify-end">
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
            Save turn limits
          </Button>
        </div>
      </CardContent>
    </Card>
  )
}
