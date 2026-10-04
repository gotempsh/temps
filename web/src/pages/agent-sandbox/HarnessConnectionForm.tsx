// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Fragment, useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { AlertTriangle, CheckCircle2, Loader2, Lock } from 'lucide-react'

import {
  importLocalAiProviderCredential,
  saveAiProviderCredential,
  verifySavedAiProviderCredential,
  type ProviderCatalogDto,
} from '@/api/client'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Textarea } from '@/components/ui/textarea'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { problemDetail } from '@/lib/api-problem'
import {
  aiProviderCatalogQueryOptions,
  publishVerifiedProvider,
} from '@/lib/ai-provider-catalog-query'
import {
  credentialVerificationMessage,
  harnessConnectionMethods,
  initialConnectionMethodId,
  OPENAI_COMPATIBLE_FORMAT,
  openAiCompatibleCredential,
  openAiCompatibleModelSelection,
  openAiCompatibleUpstreamModel,
  savedConnectionLabel,
  type HarnessConnectionMethod,
} from './harness-onboarding'

/**
 * OpenCode reads its credential from a file inside the sandbox instead of
 * going through the model relay, so it must say so wherever it is connected.
 */
const OPENCODE_CREDENTIAL_DISCLOSURE =
  'OpenCode uses a private runtime credential file. Code running as the harness user can access this credential; only use it in workspaces you trust. Refreshed tokens stay in this sandbox; after replacing the sandbox, you may need to import your local login again.'
const RELAY_CREDENTIAL_DISCLOSURE =
  'Encrypted on this Temps instance. Workspaces reach the model through a short-lived relay and never receive the credential.'

/** Render `code` spans from catalog hints such as "Run `claude setup-token`". */
function InlineCode({ text }: { text: string }) {
  return (
    <>
      {text.split('`').map((part, index) =>
        index % 2 === 1 ? (
          <code
            key={index}
            className="rounded bg-muted px-1 py-0.5 font-mono text-[0.85em]"
          >
            {part}
          </code>
        ) : (
          <Fragment key={index}>{part}</Fragment>
        )
      )}
    </>
  )
}

/**
 * Connect a harness: pick how to sign in, provide that one credential, done.
 * Temps verifies it with a small test request before saving. Once connected
 * the form collapses to a one-line summary with a Replace action.
 */
export function HarnessConnectionForm({
  provider,
  onVerificationPending,
}: {
  provider: ProviderCatalogDto
  onVerificationPending?: (pending: boolean) => void
}) {
  const queryClient = useQueryClient()
  const methods = harnessConnectionMethods(provider)
  const [methodId, setMethodId] = useState(() =>
    initialConnectionMethodId(provider, methods)
  )
  const [replacing, setReplacing] = useState(false)
  const [credential, setCredential] = useState('')
  const [credentialError, setCredentialError] = useState<string | null>(null)
  const [verificationModel, setVerificationModel] = useState(
    provider.default_model ?? ''
  )
  const savedCompatible =
    provider.credential_saved &&
    provider.auth_flavors.find(
      (flavor) => flavor.id === provider.current_auth_type
    )?.format === OPENAI_COMPATIBLE_FORMAT
  const [endpoint, setEndpoint] = useState({
    baseUrl: '',
    apiKey: '',
    model: openAiCompatibleUpstreamModel(provider.default_model),
  })
  const isOpenCode = provider.id === 'opencode'
  const stopReplacing = () => {
    setCredential('')
    setEndpoint((current) => ({ ...current, baseUrl: '', apiKey: '' }))
    setCredentialError(null)
    setReplacing(false)
  }
  const opencodeModel = isOpenCode ? verificationModel.trim() : ''

  const publish = async (updated?: ProviderCatalogDto | null) => {
    if (updated) {
      await publishVerifiedProvider(queryClient, updated)
    } else {
      // Older servers do not return the provider: wait for authoritative
      // readiness instead of treating a successful write as proof it works.
      await queryClient.invalidateQueries(
        { queryKey: aiProviderCatalogQueryOptions.queryKey },
        { throwOnError: true }
      )
    }
  }

  const pendingCallbacks = {
    onMutate: () => {
      setCredentialError(null)
      onVerificationPending?.(true)
    },
    onSettled: () => onVerificationPending?.(false),
  }

  const saveMutation = useMutation({
    ...pendingCallbacks,
    mutationFn: async (
      method: Extract<HarnessConnectionMethod, { kind: 'credential' }>
    ) => {
      const compatible = method.flavor.format === OPENAI_COMPATIBLE_FORMAT
      const verification = compatible
        ? openAiCompatibleModelSelection(endpoint.model)
        : opencodeModel
      const { data } = await saveAiProviderCredential({
        path: { provider_id: provider.id },
        body: {
          auth_type: method.flavor.id,
          credential: compatible
            ? openAiCompatibleCredential(endpoint.baseUrl, endpoint.apiKey)
            : credential.trim(),
          ...(verification ? { verification_model: verification } : {}),
        },
        throwOnError: true,
      })
      await publish(data.provider)
      return data
    },
    onSuccess: (data) => {
      toast.success(credentialVerificationMessage(data))
      stopReplacing()
    },
    onError: (error) => {
      const detail = problemDetail(
        error,
        'Could not verify and save this credential. Check your connection and try again.'
      )
      setCredentialError(detail)
      toast.error(`Failed to connect ${provider.name}`, {
        description: detail,
      })
    },
  })

  const importMutation = useMutation({
    ...pendingCallbacks,
    mutationFn: async () => {
      const { data } = await importLocalAiProviderCredential({
        path: { provider_id: provider.id },
        query: opencodeModel
          ? { verification_model: opencodeModel }
          : undefined,
        throwOnError: true,
      })
      await publish(data.provider)
      return data
    },
    onSuccess: (imported) => {
      toast.success(`${provider.name} login imported`, {
        description: credentialVerificationMessage(imported),
      })
      stopReplacing()
    },
    onError: (error) => {
      const detail = problemDetail(
        error,
        'Could not verify the local login. Authenticate the CLI as the user running Temps and try again.'
      )
      setCredentialError(detail)
      toast.error(`Could not import ${provider.name} login`, {
        description: detail,
      })
    },
  })

  const verifySavedMutation = useMutation({
    ...pendingCallbacks,
    mutationFn: async () => {
      const { data } = await verifySavedAiProviderCredential({
        path: { provider_id: provider.id },
        body: {
          verification_model: savedCompatible
            ? openAiCompatibleModelSelection(endpoint.model)
            : opencodeModel,
        },
        throwOnError: true,
      })
      return data
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
  })

  const busy =
    saveMutation.isPending ||
    importMutation.isPending ||
    verifySavedMutation.isPending

  // The catalog can change underneath the form (a host login appears after a
  // refetch), so never point the tabs at a method that no longer exists.
  const activeMethodId = methods.some((method) => method.id === methodId)
    ? methodId
    : (methods[0]?.id ?? '')

  const chooseMethod = (id: string) => {
    setMethodId(id)
    setCredential('')
    setCredentialError(null)
  }
  const activeMethod = methods.find((method) => method.id === activeMethodId)
  const activeCompatible =
    activeMethod?.kind === 'credential' &&
    activeMethod.flavor.format === OPENAI_COMPATIBLE_FORMAT
  const endpointReady = Boolean(
    endpoint.baseUrl.trim() && endpoint.apiKey.trim() && endpoint.model.trim()
  )
  // Native OpenCode auth is copied into the sandbox; an endpoint key is not.
  const relayBacked =
    !isOpenCode ||
    (provider.credential_saved && !replacing
      ? savedCompatible
      : activeCompatible)

  const endpointModelField = (
    <div className="space-y-1.5">
      <Label htmlFor={`endpoint-model-${provider.id}`}>Model</Label>
      <Input
        id={`endpoint-model-${provider.id}`}
        value={endpoint.model}
        onChange={(event) =>
          setEndpoint((current) => ({ ...current, model: event.target.value }))
        }
        placeholder="e.g. gpt-4o-mini or meta-llama/llama-3.3-70b-instruct"
        list={`endpoint-models-${provider.id}`}
        disabled={busy}
        autoComplete="off"
      />
      <datalist id={`endpoint-models-${provider.id}`}>
        {provider.models
          .map(openAiCompatibleUpstreamModel)
          .filter(Boolean)
          .map((model) => (
            <option key={model} value={model} />
          ))}
      </datalist>
    </div>
  )

  const verificationModelField = isOpenCode && (
    <div className="space-y-1.5">
      <Label htmlFor={`verification-model-${provider.id}`}>
        Model to verify
      </Label>
      <Input
        id={`verification-model-${provider.id}`}
        value={verificationModel}
        onChange={(event) => setVerificationModel(event.target.value)}
        placeholder="provider/model, e.g. anthropic/claude-sonnet-4-6"
        list={`verification-models-${provider.id}`}
        disabled={busy}
        autoComplete="off"
      />
      <datalist id={`verification-models-${provider.id}`}>
        {provider.models.map((model) => (
          <option key={model} value={model} />
        ))}
      </datalist>
    </div>
  )

  const footer = (
    <div className="space-y-1 text-xs text-muted-foreground">
      <p className="flex items-start gap-1.5">
        <Lock className="mt-0.5 size-3 shrink-0" aria-hidden="true" />
        <span>
          {relayBacked
            ? RELAY_CREDENTIAL_DISCLOSURE
            : OPENCODE_CREDENTIAL_DISCLOSURE}
        </span>
      </p>
      <p>
        {busy
          ? 'Checking the harness in a temporary sandbox…'
          : 'Temps verifies it with one small test request, which may use your provider allowance.'}
      </p>
    </div>
  )

  const errorMessage = credentialError && (
    <p role="alert" className="text-sm break-words text-destructive">
      {credentialError}
    </p>
  )

  if (provider.credential_saved && !replacing) {
    const verified = provider.credential_verification_status === 'verified'
    const method = savedConnectionLabel(provider)
    return (
      <div className="space-y-3">
        <div className="flex flex-wrap items-center justify-between gap-3 rounded-lg border p-3">
          <div className="flex min-w-0 items-start gap-2 text-sm">
            {verified && provider.workspace_ready ? (
              <CheckCircle2
                className="mt-0.5 size-4 shrink-0 text-emerald-600 dark:text-emerald-400"
                aria-hidden="true"
              />
            ) : (
              <AlertTriangle
                className="mt-0.5 size-4 shrink-0 text-amber-600 dark:text-amber-400"
                aria-hidden="true"
              />
            )}
            <div className="min-w-0">
              <p className="font-medium">
                {verified ? 'Connected' : 'Saved, not verified'}
                {method && (
                  <span className="font-normal text-muted-foreground">
                    {' '}
                    · {method}
                  </span>
                )}
              </p>
              {!(verified && provider.workspace_ready) && (
                <p role="status" className="text-muted-foreground">
                  {verified
                    ? (provider.workspace_readiness_hint ??
                      'Verified, but not usable in workspaces yet.')
                    : credentialVerificationMessage(provider)}
                </p>
              )}
            </div>
          </div>
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={busy}
            onClick={() => {
              setCredentialError(null)
              setReplacing(true)
            }}
          >
            Replace
          </Button>
        </div>
        {isOpenCode && !verified && (
          <div className="space-y-3">
            {savedCompatible ? endpointModelField : verificationModelField}
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={
                busy ||
                !(savedCompatible ? endpoint.model.trim() : opencodeModel)
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
          </div>
        )}
        {errorMessage}
      </div>
    )
  }

  if (methods.length === 0) {
    return (
      <p role="status" className="text-sm text-muted-foreground">
        {provider.name} has no connection methods on this server. Update Temps
        to connect it.
      </p>
    )
  }

  return (
    <div className="space-y-4">
      <Tabs value={activeMethodId} onValueChange={chooseMethod}>
        {(methods.length > 1 || replacing) && (
          <div className="flex flex-wrap items-center justify-between gap-2">
            {methods.length > 1 ? (
              <TabsList
                aria-label="Connection method"
                className="h-auto flex-wrap justify-start"
              >
                {methods.map((method) => (
                  <TabsTrigger
                    key={method.id}
                    value={method.id}
                    disabled={busy}
                  >
                    {method.label}
                  </TabsTrigger>
                ))}
              </TabsList>
            ) : (
              <span />
            )}
            {replacing && (
              <Button
                type="button"
                variant="ghost"
                size="sm"
                disabled={busy}
                onClick={stopReplacing}
              >
                Cancel
              </Button>
            )}
          </div>
        )}
        {methods.map((method) => (
          <TabsContent key={method.id} value={method.id} className="mt-4">
            {method.kind === 'local' ? (
              <div className="space-y-3">
                <p className="text-sm text-muted-foreground">
                  Temps found a {provider.name} login on this server (
                  {provider.local_credential?.label.toLowerCase()}). Importing
                  copies it into encrypted settings without exposing it to this
                  browser.
                </p>
                {verificationModelField}
                <Button
                  type="button"
                  size="sm"
                  disabled={busy}
                  onClick={() => importMutation.mutate()}
                >
                  {importMutation.isPending && (
                    <Loader2 className="mr-1.5 size-4 animate-spin" />
                  )}
                  {importMutation.isPending
                    ? 'Verifying…'
                    : provider.credential_saved
                      ? 'Import & replace'
                      : 'Import login'}
                </Button>
              </div>
            ) : (
              <form
                className="space-y-3"
                onSubmit={(event) => {
                  event.preventDefault()
                  const ready =
                    method.flavor.format === OPENAI_COMPATIBLE_FORMAT
                      ? endpointReady
                      : Boolean(credential.trim())
                  if (ready) saveMutation.mutate(method)
                }}
              >
                {method.flavor.format === OPENAI_COMPATIBLE_FORMAT ? (
                  <div className="space-y-3">
                    <p className="text-sm text-muted-foreground">
                      <InlineCode text={method.flavor.description} />
                    </p>
                    <div className="space-y-1.5">
                      <Label htmlFor={`endpoint-url-${provider.id}`}>
                        Base URL
                      </Label>
                      <Input
                        id={`endpoint-url-${provider.id}`}
                        type="url"
                        inputMode="url"
                        autoComplete="off"
                        disabled={busy}
                        placeholder="https://openrouter.ai/api/v1"
                        value={endpoint.baseUrl}
                        onChange={(event) =>
                          setEndpoint((current) => ({
                            ...current,
                            baseUrl: event.target.value,
                          }))
                        }
                      />
                    </div>
                    <div className="space-y-1.5">
                      <Label htmlFor={`cred-${provider.id}`}>API key</Label>
                      <Input
                        id={`cred-${provider.id}`}
                        type="password"
                        autoComplete="off"
                        disabled={busy}
                        placeholder="Paste API key"
                        value={endpoint.apiKey}
                        onChange={(event) =>
                          setEndpoint((current) => ({
                            ...current,
                            apiKey: event.target.value,
                          }))
                        }
                      />
                    </div>
                    {endpointModelField}
                  </div>
                ) : (
                  <div className="space-y-1.5">
                    {/* The selected tab already names the method. */}
                    <Label
                      htmlFor={`cred-${provider.id}`}
                      className={methods.length > 1 ? 'sr-only' : undefined}
                    >
                      {method.label}
                    </Label>
                    <p className="text-sm text-muted-foreground">
                      <InlineCode text={method.flavor.description} />
                    </p>
                    {method.flavor.format === 'config_file' ? (
                      <Textarea
                        id={`cred-${provider.id}`}
                        disabled={busy}
                        placeholder="Paste the file contents"
                        value={credential}
                        onChange={(event) => setCredential(event.target.value)}
                        className="min-h-[120px] font-mono text-xs"
                      />
                    ) : (
                      <Input
                        id={`cred-${provider.id}`}
                        type="password"
                        autoComplete="off"
                        disabled={busy}
                        placeholder={
                          method.flavor.format === 'oauth_token'
                            ? 'Paste token'
                            : 'Paste API key'
                        }
                        value={credential}
                        onChange={(event) => setCredential(event.target.value)}
                      />
                    )}
                    {method.flavor.format !== 'api_key' && (
                      <p className="text-xs text-muted-foreground">
                        No CLI yet?{' '}
                        <InlineCode text={`\`${provider.install_command}\``} />
                      </p>
                    )}
                  </div>
                )}
                {method.flavor.format !== OPENAI_COMPATIBLE_FORMAT &&
                  verificationModelField}
                <Button
                  type="submit"
                  size="sm"
                  disabled={
                    busy ||
                    !(method.flavor.format === OPENAI_COMPATIBLE_FORMAT
                      ? endpointReady
                      : credential.trim())
                  }
                >
                  {saveMutation.isPending && (
                    <Loader2 className="mr-1.5 size-4 animate-spin" />
                  )}
                  {saveMutation.isPending
                    ? 'Verifying…'
                    : provider.credential_saved
                      ? 'Verify & replace'
                      : 'Connect'}
                </Button>
              </form>
            )}
          </TabsContent>
        ))}
      </Tabs>
      {errorMessage}
      {footer}
    </div>
  )
}
