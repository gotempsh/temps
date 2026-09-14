// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Link, useParams, useSearchParams } from 'react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useEffect, useRef, useState } from 'react'
import { toast } from 'sonner'
import {
  AlertTriangle,
  ArrowLeft,
  CheckCircle2,
  Download,
  Loader2,
  Play,
  RefreshCw,
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
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import {
  activateAiProvider,
  saveAiProviderCredential,
  verifySavedAiProviderCredential,
  smokeTestAgent,
  updateAiProvider,
  type ProviderCatalogDto,
  type ProviderCatalogResponse,
} from '@/api/client'
import { AiHarnessLogo } from '@/components/ui/ai-harness-logo'
import {
  harnessSetupHref,
  harnessCheckError,
  harnessSetupStatus,
  workspaceReturnTo,
  credentialVerificationMessage,
} from './harness-onboarding'
import {
  importLocalAiProviderCredentialMutation,
  refreshAiProviderModelsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { problemDetail } from '@/lib/api-problem'
import {
  aiProviderCatalogQueryOptions,
  publishVerifiedProvider,
} from '@/lib/ai-provider-catalog-query'
import {
  isSavedProviderModelUnavailable,
  mergeProviderModelRefresh,
} from './provider-model-catalog'

export function AgentSandboxProviderDetail() {
  const { id } = useParams<{ id: string }>()
  const [params] = useSearchParams()
  const returnTo = workspaceReturnTo(params.get('returnTo'))
  usePageTitle(id ? `Provider · ${id}` : 'AI Provider')
  const { data, isPending, isError } = useQuery({
    ...aiProviderCatalogQueryOptions,
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
            <Link to={harnessSetupHref(null, returnTo)}>
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
        to={harnessSetupHref(null, returnTo)}
        className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
      >
        <ArrowLeft className="h-3.5 w-3.5" />
        All providers
      </Link>

      <ProviderEditor
        key={provider.id}
        provider={provider}
        isActive={isActive}
        returnTo={returnTo}
      />
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
  const queryClient = useQueryClient()
  const [connectionParams, setConnectionParams] = useSearchParams()
  const usesConnectionCards =
    (provider.id === 'codex_cli' || provider.id === 'claude_cli') &&
    provider.auth_flavors.length > 0
  const requestedMethod = connectionParams.get('connectionMethod')
  const connectionMethod =
    usesConnectionCards &&
    (provider.auth_flavors.some((flavor) => flavor.id === requestedMethod) ||
      (provider.id === 'codex_cli' && requestedMethod === 'local'))
      ? requestedMethod
      : null
  const chooseConnectionMethod = (method: string | null) => {
    setCredential('')
    setCredentialError(null)
    if (method && method !== 'local') setSelectedFlavorId(method)
    setConnectionParams((current) => {
      const next = new URLSearchParams(current)
      if (method) next.set('connectionMethod', method)
      else next.delete('connectionMethod')
      return next
    })
  }
  const defaultFlavor = provider.auth_flavors.find(
    (f) => f.id === provider.current_auth_type
  ) ??
    provider.auth_flavors[0] ?? {
      id: '',
      label: 'Credential',
      description: 'No authentication methods are available for this harness.',
      format: 'api_key',
      env_var: null,
    }

  const [flavorDraft, setSelectedFlavorId] = useState(defaultFlavor.id)
  const selectedFlavorId =
    usesConnectionCards && connectionMethod && connectionMethod !== 'local'
      ? connectionMethod
      : flavorDraft
  const [credentialDraft, setCredentialDraft] = useState({
    method: connectionMethod,
    value: '',
  })
  const credential =
    credentialDraft.method === connectionMethod ? credentialDraft.value : ''
  const setCredential = (value: string) =>
    setCredentialDraft({ method: connectionMethod, value })
  const [savingCredential, setSaving] = useState(false)
  const [credentialError, setCredentialError] = useState<string | null>(null)
  const [verificationModel, setVerificationModel] = useState(
    provider.default_model ?? ''
  )
  const verifySavedMutation = useMutation({
    mutationFn: async () => {
      const { data } = await verifySavedAiProviderCredential({
        path: { provider_id: provider.id },
        body: { verification_model: verificationModel.trim() },
        throwOnError: true,
      })
      return data
    },
    onMutate: () => {
      setCredentialError(null)
      onVerificationPending?.(true)
    },
    onSuccess: async (data) => {
      if (data.provider)
        await publishVerifiedProvider(queryClient, data.provider)
      toast.success(credentialVerificationMessage(data))
    },
    onError: (error) => {
      const detail = problemDetail(
        error,
        'Could not verify this model. Choose a model your saved account can access and retry.'
      )
      setCredentialError(detail)
      toast.error('Model verification failed', { description: detail })
    },
    onSettled: () => onVerificationPending?.(false),
  })
  const saving = savingCredential || verifySavedMutation.isPending
  const [activating, setActivating] = useState(false)
  const [testing, setTesting] = useState(false)
  const [testResult, setTestResult] = useState<{
    passed: boolean
    environment: string
    cli_version?: string | null
    auth_info?: string | null
    setup_hint?: string | null
    detail?: string | null
  } | null>(null)

  const initialModel = provider.default_model ?? ''
  const [serverModel, setServerModel] = useState(initialModel)
  const [modelDraft, setModelDraft] = useState(initialModel)
  const [customMode, setCustomMode] = useState(
    provider.models.length === 0 ||
      (initialModel !== '' && !provider.models.includes(initialModel))
  )
  const [savingModel, setSavingModel] = useState(false)
  const refreshModelsMutation = useMutation(refreshAiProviderModelsMutation())
  const importLocalCredentialMutation = useMutation({
    ...importLocalAiProviderCredentialMutation(),
    // The catch below owns the inline error and its single notification.
    onError: () => {},
  })

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

  const selectedFlavor =
    provider.auth_flavors.find((f) => f.id === selectedFlavorId) ??
    defaultFlavor

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
      setTestResult(null)
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
      await activateAiProvider({
        path: { provider_id: provider.id },
        throwOnError: true,
      })
      toast.success(`${provider.name} is now the active provider`)
      await Promise.all([
        queryClient.invalidateQueries({
          queryKey: aiProviderCatalogQueryOptions.queryKey,
        }),
        queryClient.invalidateQueries({ queryKey: ['platform-settings'] }),
      ])
    } catch (e) {
      toast.error(`Failed to activate ${provider.name}`, {
        description: problemDetail(
          e,
          'The request failed. Check your connection and permissions, then retry.'
        ),
      })
    } finally {
      setActivating(false)
    }
  }

  const handleTest = async () => {
    setTesting(true)
    setTestResult(null)
    try {
      const { data } = await smokeTestAgent({
        path: { project_id: 0 },
        query: { provider_id: provider.id },
        throwOnError: true,
      })
      setTestResult(data)
      if (data.passed) {
        toast.success(`${provider.name} environment check passed`)
      } else {
        toast.error(`${provider.name} test failed`, {
          description: data.setup_hint ?? 'See card for details',
        })
      }
    } catch (e) {
      const detail = harnessCheckError(e)
      setTestResult({
        passed: false,
        environment: 'unknown',
        setup_hint: detail,
      })
      toast.error('Environment check failed', { description: detail })
    } finally {
      setTesting(false)
    }
  }

  const handleSave = async () => {
    if (!credential.trim()) return
    setSaving(true)
    setCredentialError(null)
    onVerificationPending?.(true)
    try {
      const { data } = await saveAiProviderCredential({
        path: { provider_id: provider.id },
        body: {
          auth_type: selectedFlavor.id,
          credential: credential.trim(),
          ...(provider.id === 'opencode' && verificationModel.trim()
            ? { verification_model: verificationModel.trim() }
            : {}),
        },
        throwOnError: true,
      })
      setTestResult(null)
      if (data.provider) {
        await publishVerifiedProvider(queryClient, data.provider)
      } else {
        // Support older servers, but wait for authoritative readiness instead
        // of treating a successful write as proof that the harness can run.
        await queryClient.invalidateQueries(
          {
            queryKey: aiProviderCatalogQueryOptions.queryKey,
          },
          { throwOnError: true }
        )
      }
      toast.success(credentialVerificationMessage(data))
      setCredential('')
    } catch (e) {
      const detail = problemDetail(
        e,
        'Could not verify and save this credential. Check your connection and try again.'
      )
      setCredentialError(detail)
      toast.error(`Failed to save ${provider.name} credential`, {
        description: detail,
      })
    } finally {
      setSaving(false)
      onVerificationPending?.(false)
    }
  }

  const handleImportLocalCredential = async () => {
    setSaving(true)
    setCredentialError(null)
    onVerificationPending?.(true)
    try {
      const imported = await importLocalCredentialMutation.mutateAsync({
        path: { provider_id: provider.id },
        query:
          provider.id === 'opencode' && verificationModel.trim()
            ? { verification_model: verificationModel.trim() }
            : undefined,
      })
      setTestResult(null)
      setSelectedFlavorId(imported.auth_type)
      setCredential('')
      if (imported.provider) {
        await publishVerifiedProvider(queryClient, imported.provider)
      } else {
        await queryClient.invalidateQueries(
          {
            queryKey: aiProviderCatalogQueryOptions.queryKey,
          },
          { throwOnError: true }
        )
      }
      toast.success(`${provider.name} local login imported`, {
        description: credentialVerificationMessage(imported),
      })
    } catch (cause) {
      const detail = problemDetail(
        cause,
        'Could not verify the local login. Authenticate the CLI as the user running Temps and try again.'
      )
      setCredentialError(detail)
      toast.error(`Could not import ${provider.name} local login`, {
        description: detail,
      })
    } finally {
      setSaving(false)
      onVerificationPending?.(false)
    }
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex items-center gap-3">
          <AiHarnessLogo providerId={provider.id} size={32} />
          <div>
            <h2 className="text-lg font-semibold">{provider.name}</h2>
            <p className="text-sm text-muted-foreground">
              {harnessSetupStatus(provider)}
            </p>
          </div>
        </div>
        {!embedded && (
          <Button asChild variant="outline" size="sm">
            <Link to={returnTo}>Back to workspace</Link>
          </Button>
        )}
      </div>
      <p className="text-sm text-muted-foreground">
        {embedded
          ? 'Connect once. Reuse this account across your workspaces.'
          : 'Setup is saved on this Temps instance. You can leave and return without losing saved credentials or model settings.'}
      </p>

      <Card
        className={embedded ? 'border-0 shadow-none rounded-none' : undefined}
      >
        <CardHeader className={embedded ? 'p-0 pb-4' : undefined}>
          <div className="flex flex-wrap items-start justify-between gap-3">
            <div>
              {!embedded && (
                <CardTitle className="text-base">
                  1. Connect your account
                </CardTitle>
              )}
              <CardDescription>
                {embedded && provider.id !== 'opencode'
                  ? 'Your credential is encrypted and saved on this Temps instance.'
                  : provider.id === 'opencode'
                    ? 'OpenCode uses a private runtime credential file. Code running as the harness user can access this credential; only use it in workspaces you trust. Refreshed tokens stay in this sandbox; after replacing the sandbox, you may need to import your local login again.'
                    : 'Encrypted with AES-256-GCM at rest. Workspace-capable providers use it only through a short-lived server relay; the reusable credential is never injected into the sandbox.'}
              </CardDescription>
            </div>
            {!usesConnectionCards &&
              provider.id !== 'claude_cli' &&
              provider.local_credential && (
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() => void handleImportLocalCredential()}
                  disabled={saving || importLocalCredentialMutation.isPending}
                >
                  {importLocalCredentialMutation.isPending ? (
                    <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" />
                  ) : (
                    <Download className="mr-1.5 h-3.5 w-3.5" />
                  )}
                  {importLocalCredentialMutation.isPending
                    ? 'Importing…'
                    : provider.credential_saved
                      ? 'Replace with local login'
                      : 'Use local login'}
                </Button>
              )}
          </div>
        </CardHeader>
        <CardContent className={embedded ? 'space-y-4 p-0' : 'space-y-4'}>
          {provider.id === 'opencode' && (
            <div className="space-y-2">
              <Label htmlFor={`verification-model-${provider.id}`}>
                Model to verify
              </Label>
              <Input
                id={`verification-model-${provider.id}`}
                value={verificationModel}
                onChange={(event) => setVerificationModel(event.target.value)}
                placeholder="provider/model"
                list={`verification-models-${provider.id}`}
                disabled={saving || verifySavedMutation.isPending}
                autoComplete="off"
              />
              <datalist id={`verification-models-${provider.id}`}>
                {provider.models.map((model) => (
                  <option key={model} value={model} />
                ))}
              </datalist>
              <p className="text-xs text-muted-foreground">
                Choose a model your account can access. Verification runs a
                small request in a temporary sandbox and may use your provider
                allowance.
              </p>
              {provider.credential_saved && (
                <>
                  <p role="status" className="text-sm text-muted-foreground">
                    {credentialVerificationMessage(provider)}
                  </p>
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    disabled={
                      saving ||
                      verifySavedMutation.isPending ||
                      !verificationModel.trim()
                    }
                    onClick={() => verifySavedMutation.mutate()}
                  >
                    {verifySavedMutation.isPending && (
                      <Loader2 className="mr-1.5 size-4 animate-spin" />
                    )}
                    {verifySavedMutation.isPending
                      ? 'Verifying model…'
                      : 'Verify saved login'}
                  </Button>
                </>
              )}
            </div>
          )}
          {usesConnectionCards && !connectionMethod && (
            <div
              className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3"
              aria-label="Connection methods"
            >
              {provider.id === 'codex_cli' && (
                <button
                  type="button"
                  className="rounded-lg border p-4 text-left hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                  onClick={() => chooseConnectionMethod('local')}
                >
                  <Download className="mb-3 size-5" />
                  <p className="font-medium">Use local login</p>
                  <p className="mt-1 text-sm text-muted-foreground">
                    {provider.local_credential
                      ? 'Detected on the Temps host. Verify and import it securely.'
                      : 'Connect using a Codex login on the Temps host.'}
                  </p>
                </button>
              )}
              {provider.auth_flavors.map((flavor) => (
                <button
                  key={flavor.id}
                  type="button"
                  className="rounded-lg border p-4 text-left hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                  onClick={() => chooseConnectionMethod(flavor.id)}
                >
                  <AiHarnessLogo providerId={provider.id} size={20} />
                  <p className="mt-3 font-medium">
                    {flavor.format === 'api_key'
                      ? provider.id === 'codex_cli'
                        ? 'OpenAI API key'
                        : 'Anthropic API key'
                      : 'Subscription'}
                  </p>
                  <p className="mt-1 text-sm text-muted-foreground">
                    {flavor.format === 'api_key'
                      ? 'Connect with a pay-as-you-go API key.'
                      : provider.id === 'codex_cli'
                        ? 'Paste your ChatGPT subscription auth.json.'
                        : 'Paste a token from claude setup-token.'}
                  </p>
                </button>
              ))}
            </div>
          )}
          {usesConnectionCards && connectionMethod && (
            <Button
              type="button"
              variant="ghost"
              size="sm"
              disabled={saving || importLocalCredentialMutation.isPending}
              onClick={() => chooseConnectionMethod(null)}
            >
              Choose another method
            </Button>
          )}
          {usesConnectionCards && connectionMethod === 'local' && (
            <div className="space-y-3">
              <h3 className="font-medium">Use local Codex login</h3>
              <p className="text-sm text-muted-foreground">
                {provider.local_credential
                  ? 'A login was detected on the Temps host. Verification imports it without exposing the credential to this browser.'
                  : 'Run codex login on the Temps host as the user running Temps, then reload to detect it.'}
              </p>
              <Button
                type="button"
                disabled={
                  !provider.local_credential ||
                  saving ||
                  importLocalCredentialMutation.isPending
                }
                onClick={() => void handleImportLocalCredential()}
              >
                {importLocalCredentialMutation.isPending
                  ? 'Verifying…'
                  : provider.credential_saved
                    ? 'Verify & replace login'
                    : 'Verify & connect'}
              </Button>
              {credentialError && (
                <p role="alert" className="text-sm text-destructive">
                  {credentialError}
                </p>
              )}
            </div>
          )}
          {(!usesConnectionCards ||
            (connectionMethod && connectionMethod !== 'local')) && (
            <>
              {!embedded && !usesConnectionCards && (
                <p className="text-sm text-muted-foreground">
                  Run login commands on the machine hosting Temps, as the
                  operating-system user running Temps—not in the workspace
                  terminal. Then import the detected login or paste a credential
                  below.
                </p>
              )}
              <details
                open={
                  !usesConnectionCards &&
                  !embedded &&
                  !provider.credential_saved
                }
                className={
                  embedded ? 'text-sm' : 'rounded-md border p-3 text-sm'
                }
              >
                <summary className="cursor-pointer font-medium">
                  {embedded
                    ? 'How do I get a credential?'
                    : 'Login instructions'}
                </summary>
                <div className="space-y-2 pt-3">
                  {embedded && provider.id !== 'claude_cli' && (
                    <p className="text-muted-foreground">
                      Run these commands on the Temps host as the user running
                      Temps, not in the workspace terminal.
                    </p>
                  )}
                  <p className="text-muted-foreground">
                    Install the CLI if needed:
                  </p>
                  <pre className="overflow-x-auto rounded bg-muted p-2 text-xs">
                    {provider.install_command}
                  </pre>
                  <p className="text-muted-foreground">
                    {provider.id === 'claude_cli'
                      ? 'Create a Claude token, then paste it below (or use an Anthropic API key):'
                      : 'Authenticate, then reload this page to detect the local login:'}
                  </p>
                  <pre className="overflow-x-auto rounded bg-muted p-2 text-xs">
                    {provider.id === 'claude_cli'
                      ? 'claude setup-token'
                      : provider.auth_command}
                  </pre>
                </div>
              </details>
              {!embedded &&
                !usesConnectionCards &&
                provider.id === 'claude_cli' && (
                  <p className="text-sm text-muted-foreground">
                    Local-login import is not supported for Claude Code. Paste a
                    token from <code>claude setup-token</code> or an Anthropic
                    API key below.
                  </p>
                )}
              {provider.id !== 'claude_cli' && provider.local_credential && (
                <div className="flex items-start gap-2 rounded-md border border-emerald-500/30 bg-emerald-500/5 px-3 py-2 text-xs text-emerald-800 dark:text-emerald-200">
                  <CheckCircle2 className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                  <span>
                    Temps found a local {provider.name} credential in{' '}
                    {provider.local_credential.label.toLowerCase()}. Importing
                    it copies the credential directly into encrypted settings
                    without exposing it to this browser.
                  </span>
                </div>
              )}
              <Tabs
                value={selectedFlavorId}
                onValueChange={setSelectedFlavorId}
              >
                {!usesConnectionCards && provider.auth_flavors.length > 1 && (
                  <TabsList
                    aria-label="Authentication method"
                    className="h-auto flex-wrap justify-start"
                  >
                    {provider.auth_flavors.map((flavor) => (
                      <TabsTrigger
                        key={flavor.id}
                        value={flavor.id}
                        disabled={saving}
                      >
                        {flavor.label}
                      </TabsTrigger>
                    ))}
                  </TabsList>
                )}

                <TabsContent
                  value={selectedFlavorId}
                  className="space-y-2 mt-4"
                >
                  <Label htmlFor={`cred-${provider.id}`}>
                    {selectedFlavor.id
                      ? `${selectedFlavor.label} credential`
                      : 'Credential'}
                  </Label>
                  <p className="text-xs text-muted-foreground">
                    {selectedFlavor.description}
                  </p>
                  {selectedFlavor.format === 'config_file' ? (
                    <Textarea
                      id={`cred-${provider.id}`}
                      disabled={saving}
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
                      disabled={saving}
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
                  <div className="flex flex-wrap items-center justify-between gap-3">
                    <p className="text-sm text-muted-foreground">
                      {saving
                        ? 'Checking the harness in a temporary sandbox…'
                        : 'Verification sends a small test request and may use your provider allowance.'}
                    </p>
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      onClick={handleSave}
                      disabled={
                        saving || !credential.trim() || !selectedFlavor.id
                      }
                    >
                      {saving ? (
                        <Loader2 className="h-3.5 w-3.5 animate-spin mr-1.5" />
                      ) : (
                        <Save className="h-3.5 w-3.5 mr-1.5" />
                      )}
                      {saving ? 'Verifying…' : 'Verify & save'}
                    </Button>
                  </div>
                  {credentialError && (
                    <p
                      role="alert"
                      className="text-sm text-destructive break-words"
                    >
                      {credentialError}
                    </p>
                  )}
                </TabsContent>
              </Tabs>
            </>
          )}
          {!embedded && !usesConnectionCards && (
            <div className="space-y-2">
              <p className="text-sm text-muted-foreground">
                Optional: check the CLI installation and authentication in the
                configured execution environment. This does not verify a reply
                in your persistent workspace.
              </p>
              <Button
                variant="outline"
                size="sm"
                onClick={() => void handleTest()}
                disabled={testing || !provider.credential_saved}
              >
                {testing ? (
                  <Loader2 className="size-4 animate-spin" />
                ) : (
                  <Play className="size-4" />
                )}{' '}
                {testing ? 'Checking…' : 'Check environment'}
              </Button>
            </div>
          )}
          {testResult && (
            <div
              role="status"
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
                {testResult.passed
                  ? 'Environment check passed'
                  : 'Environment check failed'}
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
              {testResult.detail && (
                <pre className="whitespace-pre-wrap break-words text-xs">
                  {testResult.detail}
                </pre>
              )}
              {testResult.setup_hint && (
                <p className="text-muted-foreground">{testResult.setup_hint}</p>
              )}
            </div>
          )}
        </CardContent>
      </Card>

      {!embedded && (
        <>
          <Card>
            <CardHeader>
              <div className="flex flex-wrap items-start justify-between gap-3">
                <div>
                  <CardTitle className="text-base">2. Choose a model</CardTitle>
                  <CardDescription>
                    {provider.workspace_ready
                      ? 'Leave blank to let the CLI pick. Refresh checks which models the saved workspace credential can run inside a short-lived isolated sandbox.'
                      : provider.id === 'opencode'
                        ? 'OpenCode resolves models from its configured providers. Refresh asks the authenticated CLI on the Temps host for the current list.'
                        : 'Leave blank to let the CLI pick. Refresh asks the authenticated CLI installed on the Temps host which models this account can run.'}
                  </CardDescription>
                  <p className="mt-1 text-xs text-muted-foreground">
                    {provider.models_refreshed_at
                      ? `Last refreshed ${new Date(provider.models_refreshed_at).toLocaleString()} · ${provider.model_source.replace('_', ' ')}`
                      : provider.workspace_ready
                        ? 'Bootstrap catalog — not yet verified with the saved workspace credential.'
                        : 'Bootstrap catalog — not yet verified against the authenticated host CLI.'}
                  </p>
                </div>
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
                    {refreshModelsMutation.isPending
                      ? 'Refreshing…'
                      : 'Refresh models'}
                  </Button>
                </div>
              </div>
            </CardHeader>
            <CardContent className="space-y-3">
              {savedModelUnavailable && (
                <div
                  role="alert"
                  className="flex items-start gap-2 rounded-md border border-amber-500/30 bg-amber-500/5 px-3 py-2 text-xs text-amber-800 dark:text-amber-200"
                >
                  <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                  <span>
                    The saved model <code>{serverModel}</code> was not reported
                    by the refreshed CLI. Choose an available model or use the
                    provider default before starting another turn.
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
                  <SelectTrigger id={`model-${provider.id}`}>
                    <SelectValue placeholder="Use provider default" />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="_default">
                      Use provider default
                    </SelectItem>
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

          <Card>
            <CardHeader>
              <CardTitle className="text-base">
                3. Verify your first workspace reply
              </CardTitle>
              <CardDescription>
                Return to your workspace, select this harness, and send “Reply
                with OK. Do not run tools.” A successful reply verifies the
                selected model and workspace together. This may use your
                provider allowance.
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-3">
              <p className="text-sm text-muted-foreground">
                For your first task, use Ask each time to review tool approvals.
                Auto lets the harness run tools without per-command approval
                inside the workspace; choose it only when you trust the task.
              </p>
              <Button asChild variant="outline" size="sm">
                <Link to={returnTo}>Open workspace to verify</Link>
              </Button>
            </CardContent>
          </Card>
          <details className="rounded-lg border p-4">
            <summary className="cursor-pointer text-sm font-medium">
              Advanced: instance default and autofix limits
            </summary>
            <div className="space-y-4 pt-4">
              <p className="text-sm text-muted-foreground">
                The instance default affects server-side workflows. It does not
                change the harness selected in an existing workspace thread.
              </p>
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
              <TurnLimitsCard provider={provider} />
            </div>
          </details>
        </>
      )}
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
