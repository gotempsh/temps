import {
  createProviderKey,
  deleteProviderKey,
  listProviderKeys,
  testProviderKeyById,
  testProviderKeyInline,
  updateProviderKey,
  type ProviderKeyResponse,
} from '@/api/client'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
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
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  AI_PROVIDERS,
  AiProviderIcon,
  type AiProviderMeta,
} from '@/lib/ai-providers'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  Check,
  ChevronDown,
  ExternalLink,
  Info,
  Loader2,
  Sparkles,
  Trash2,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'

const KEYS_QUERY_KEY = ['providerKeys'] as const

/**
 * Settings → AI Providers. Bring-your-own-key configuration for the four
 * supported chat providers (OpenAI, Anthropic, xAI, Google Gemini). Keys are
 * validated against the provider before they're stored (encrypted), and the
 * first active key powers every AI feature — deployment debugging, alert
 * investigation, humanized alerts, and so on.
 */
export function AiProvidersPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  usePageTitle('AI Providers')

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'AI Providers' },
    ])
  }, [setBreadcrumbs])

  const { data: keys, isLoading } = useQuery({
    queryKey: KEYS_QUERY_KEY,
    queryFn: async () => (await listProviderKeys()).data ?? [],
  })

  // The backend's model resolver uses the first active key, so surface which
  // provider is actually serving requests right now.
  const activeKeyId = (keys ?? []).find((k) => k.is_active)?.id ?? null

  return (
    <div className="space-y-6">
      <div className="space-y-1">
        <h1 className="flex items-center gap-2 text-2xl font-semibold">
          <Sparkles className="h-6 w-6 text-primary" />
          AI Providers
        </h1>
        <p className="text-sm text-muted-foreground">
          Connect an AI provider to power deployment debugging, alert
          investigation, and humanized alerts. Bring your own key — it's
          verified, then stored encrypted, and never leaves your server except
          to call the provider you chose.
        </p>
      </div>

      <Alert>
        <Info className="h-4 w-4" />
        <AlertTitle>How the active provider is chosen</AlertTitle>
        <AlertDescription>
          You can connect more than one provider. The first one marked{' '}
          <span className="font-medium">Active</span> is used for every AI
          feature. Toggle Active to switch between configured providers.
        </AlertDescription>
      </Alert>

      {isLoading ? (
        <div className="space-y-4">
          {AI_PROVIDERS.map((p) => (
            <Skeleton key={p.id} className="h-28 w-full" />
          ))}
        </div>
      ) : (
        <div className="space-y-4">
          {AI_PROVIDERS.map((meta) => (
            <ProviderConfigCard
              key={meta.id}
              meta={meta}
              existing={(keys ?? []).find((k) => k.provider === meta.id)}
              inUse={
                activeKeyId !== null &&
                (keys ?? []).find((k) => k.provider === meta.id)?.id ===
                  activeKeyId
              }
            />
          ))}
        </div>
      )}
    </div>
  )
}

function ProviderConfigCard({
  meta,
  existing,
  inUse,
}: {
  meta: AiProviderMeta
  existing?: ProviderKeyResponse
  inUse: boolean
}) {
  const queryClient = useQueryClient()
  const [editing, setEditing] = useState(false)
  const [apiKey, setApiKey] = useState('')
  const [baseUrl, setBaseUrl] = useState('')
  const [showAdvanced, setShowAdvanced] = useState(false)
  const [testResult, setTestResult] = useState<{
    ok: boolean
    message: string
  } | null>(null)
  const [testing, setTesting] = useState(false)

  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: KEYS_QUERY_KEY })

  const resetForm = () => {
    setApiKey('')
    setBaseUrl('')
    setShowAdvanced(false)
    setTestResult(null)
    setEditing(false)
  }

  const saveMutation = useMutation({
    mutationFn: async () => {
      const trimmedBase = baseUrl.trim() || undefined
      if (existing) {
        return updateProviderKey({
          path: { id: existing.id },
          body: { api_key: apiKey.trim(), base_url: trimmedBase ?? null },
          throwOnError: true,
        })
      }
      return createProviderKey({
        body: {
          provider: meta.id,
          display_name: meta.name,
          api_key: apiKey.trim(),
          base_url: trimmedBase,
        },
        throwOnError: true,
      })
    },
    onSuccess: () => {
      invalidate()
      resetForm()
      toast.success(`${meta.name} ${existing ? 'key updated' : 'connected'}`)
    },
    onError: (err: unknown) => {
      const detail =
        (err as { body?: { detail?: string } })?.body?.detail ??
        (err as Error)?.message ??
        'Could not save the key.'
      toast.error(detail)
    },
  })

  const deleteMutation = useMutation({
    mutationFn: () =>
      deleteProviderKey({ path: { id: existing!.id }, throwOnError: true }),
    onSuccess: () => {
      invalidate()
      toast.success(`${meta.name} disconnected`)
    },
    meta: { errorTitle: `Failed to disconnect ${meta.name}` },
  })

  const toggleMutation = useMutation({
    mutationFn: (is_active: boolean) =>
      updateProviderKey({
        path: { id: existing!.id },
        body: { is_active },
        throwOnError: true,
      }),
    onSuccess: invalidate,
    meta: { errorTitle: `Failed to update ${meta.name}` },
  })

  // Test the typed key before saving (no persistence).
  const runInlineTest = async () => {
    setTesting(true)
    setTestResult(null)
    try {
      const res = await testProviderKeyInline({
        body: {
          provider: meta.id,
          api_key: apiKey.trim(),
          base_url: baseUrl.trim() || undefined,
        },
        throwOnError: true,
      })
      const ok = res.data?.success ?? false
      setTestResult({
        ok,
        message: ok
          ? `Key works (${res.data?.latency_ms ?? 0} ms)`
          : (res.data?.error ?? 'The key was rejected by the provider.'),
      })
    } catch (err: unknown) {
      setTestResult({
        ok: false,
        message:
          (err as { body?: { detail?: string } })?.body?.detail ??
          (err as Error)?.message ??
          'Test failed.',
      })
    } finally {
      setTesting(false)
    }
  }

  // Test the already-stored key.
  const [testingStored, setTestingStored] = useState(false)
  const runStoredTest = async () => {
    if (!existing) return
    setTestingStored(true)
    try {
      const res = await testProviderKeyById({
        path: { id: existing.id },
        throwOnError: true,
      })
      if (res.data?.success) {
        toast.success(`${meta.name} key is valid (${res.data.latency_ms} ms)`)
      } else {
        toast.error(res.data?.error ?? `${meta.name} key was rejected.`)
      }
    } catch (err: unknown) {
      toast.error(
        (err as { body?: { detail?: string } })?.body?.detail ??
          `Could not reach ${meta.name}.`
      )
    } finally {
      setTestingStored(false)
    }
  }

  const showForm = editing || !existing
  const canSave = apiKey.trim().length > 0 && !saveMutation.isPending

  return (
    <Card>
      <CardHeader>
        <div className="flex items-start justify-between gap-3">
          <div className="flex items-start gap-3">
            <AiProviderIcon provider={meta.id} size={40} />
            <div className="space-y-0.5">
              <CardTitle className="text-base">{meta.name}</CardTitle>
              <CardDescription>{meta.tagline}</CardDescription>
              <p className="text-xs text-muted-foreground">{meta.models}</p>
            </div>
          </div>
          <StatusBadge existing={existing} inUse={inUse} />
        </div>
      </CardHeader>
      <CardContent className="space-y-4">
        {existing && !editing && (
          <div className="space-y-4">
            <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
              <div className="font-mono text-sm text-muted-foreground">
                ••••••••••••{existing.base_url ? ` · ${existing.base_url}` : ''}
              </div>
              <div className="flex items-center gap-2">
                <Label
                  htmlFor={`active-${meta.id}`}
                  className="text-sm text-muted-foreground"
                >
                  Active
                </Label>
                <Switch
                  id={`active-${meta.id}`}
                  checked={existing.is_active}
                  disabled={toggleMutation.isPending}
                  onCheckedChange={(c) => toggleMutation.mutate(c)}
                />
              </div>
            </div>
            <div className="flex flex-wrap gap-2">
              <Button
                variant="outline"
                size="sm"
                onClick={runStoredTest}
                disabled={testingStored}
              >
                {testingStored ? (
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                ) : (
                  <Check className="mr-2 h-4 w-4" />
                )}
                Test connection
              </Button>
              <Button
                variant="outline"
                size="sm"
                onClick={() => {
                  setEditing(true)
                  setTestResult(null)
                }}
              >
                Replace key
              </Button>
              <Button
                variant="ghost"
                size="sm"
                className="text-destructive hover:text-destructive"
                onClick={() => deleteMutation.mutate()}
                disabled={deleteMutation.isPending}
              >
                {deleteMutation.isPending ? (
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                ) : (
                  <Trash2 className="mr-2 h-4 w-4" />
                )}
                Remove
              </Button>
            </div>
          </div>
        )}

        {showForm && (
          <div className="space-y-3">
            <div className="space-y-1.5">
              <div className="flex items-center justify-between">
                <Label htmlFor={`key-${meta.id}`}>API key</Label>
                <a
                  href={meta.keyDocsUrl}
                  target="_blank"
                  rel="noreferrer"
                  className="inline-flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
                >
                  Where do I find my key?
                  <ExternalLink className="h-3 w-3" />
                </a>
              </div>
              <Input
                id={`key-${meta.id}`}
                type="password"
                autoComplete="off"
                placeholder={`Paste your ${meta.name} API key`}
                value={apiKey}
                onChange={(e) => {
                  setApiKey(e.target.value)
                  setTestResult(null)
                }}
              />
            </div>

            <button
              type="button"
              onClick={() => setShowAdvanced((s) => !s)}
              className="inline-flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
            >
              <ChevronDown
                className={`h-3.5 w-3.5 transition-transform ${showAdvanced ? 'rotate-180' : ''}`}
              />
              Advanced
            </button>
            {showAdvanced && (
              <div className="space-y-1.5">
                <Label htmlFor={`base-${meta.id}`}>Custom base URL</Label>
                <Input
                  id={`base-${meta.id}`}
                  type="url"
                  placeholder="Optional — e.g. a proxy or self-hosted gateway"
                  value={baseUrl}
                  onChange={(e) => setBaseUrl(e.target.value)}
                />
                <p className="text-xs text-muted-foreground">
                  Leave blank to use {meta.name}'s default endpoint.
                </p>
              </div>
            )}

            {testResult && (
              <p
                className={`text-sm ${testResult.ok ? 'text-green-600 dark:text-green-500' : 'text-destructive'}`}
              >
                {testResult.message}
              </p>
            )}

            <div className="flex flex-wrap gap-2">
              <Button
                onClick={() => saveMutation.mutate()}
                disabled={!canSave}
                size="sm"
              >
                {saveMutation.isPending ? (
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                ) : (
                  <Check className="mr-2 h-4 w-4" />
                )}
                {existing ? 'Save new key' : 'Connect'}
              </Button>
              <Button
                variant="outline"
                size="sm"
                onClick={runInlineTest}
                disabled={!apiKey.trim() || testing}
              >
                {testing ? (
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                ) : null}
                Test key
              </Button>
              {existing && (
                <Button variant="ghost" size="sm" onClick={resetForm}>
                  Cancel
                </Button>
              )}
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  )
}

function StatusBadge({
  existing,
  inUse,
}: {
  existing?: ProviderKeyResponse
  inUse: boolean
}) {
  if (!existing) {
    return (
      <Badge variant="outline" className="shrink-0 text-muted-foreground">
        Not configured
      </Badge>
    )
  }
  if (inUse) {
    return (
      <Badge className="shrink-0 gap-1 bg-green-600 hover:bg-green-700">
        <Check className="h-3 w-3" />
        In use
      </Badge>
    )
  }
  if (existing.is_active) {
    return (
      <Badge variant="secondary" className="shrink-0">
        Active
      </Badge>
    )
  }
  return (
    <Badge variant="outline" className="shrink-0 text-muted-foreground">
      Connected · inactive
    </Badge>
  )
}
