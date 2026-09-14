// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useEffect, useRef, useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { refreshAiProviderModelsMutation } from '@/api/client/@tanstack/react-query.gen'
import { aiProviderCatalogQueryOptions } from '@/lib/ai-provider-catalog-query'
import { mergeProviderModelRefresh } from '@/pages/agent-sandbox/provider-model-catalog'
import { problemDetail } from '@/lib/api-problem'
import type { ProviderCatalogResponse } from '@/api/client'
import type { ProviderCatalogDto } from '@/api/client'
import { ProviderEditor } from '@/pages/agent-sandbox/AgentSandboxProviderDetail'
import { Button } from '@/components/ui/button'
import { Label } from '@/components/ui/label'
import { SearchableSelect } from '@/components/ui/searchable-select'
import { AiHarnessLogo } from '@/components/ui/ai-harness-logo'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  chatHarnessProviderOptions,
  resolveChatRuntimeSelection,
  providerCatalogNeedsRefresh,
  type ChatRuntimeSelection,
} from '@/components/ai/chat-runtime-options'

export function WorkspaceHarnessSetup({
  provider,
  selection,
  onChange,
  mode,
  onVerificationPending,
}: {
  provider: ProviderCatalogDto
  selection: ChatRuntimeSelection
  onChange: (selection: ChatRuntimeSelection) => void
  mode: 'connection' | 'model'
  onVerificationPending?: (pending: boolean) => void
}) {
  const [editing, setEditing] = useState(false)
  const queryClient = useQueryClient()
  const { mutateAsync, isPending } = useMutation(
    refreshAiProviderModelsMutation()
  )
  const [modelError, setModelError] = useState<string | null>(null)
  const attempted = useRef(new Set<string>())
  const options = chatHarnessProviderOptions([provider])
  const needsDiscovery = options[0]
    ? providerCatalogNeedsRefresh(options[0])
    : false
  const refreshModels = useCallback(async () => {
    setModelError(null)
    try {
      const refreshed = await mutateAsync({
        path: { provider_id: provider.id },
      })
      await queryClient.cancelQueries({
        queryKey: aiProviderCatalogQueryOptions.queryKey,
      })
      queryClient.setQueryData<ProviderCatalogResponse>(
        aiProviderCatalogQueryOptions.queryKey,
        (catalog) =>
          catalog ? mergeProviderModelRefresh(catalog, refreshed) : catalog
      )
      if (
        !refreshed.runtime_models.length ||
        !['live', 'cache'].includes(refreshed.model_source)
      ) {
        setModelError(
          'The harness did not return a current model list. Retry model discovery.'
        )
      }
    } catch (error) {
      setModelError(
        problemDetail(
          error,
          'Could not load models. Check the connection and retry.'
        )
      )
    }
  }, [mutateAsync, provider.id, queryClient])
  useEffect(() => {
    if (
      mode !== 'model' ||
      !needsDiscovery ||
      attempted.current.has(provider.id)
    )
      return
    attempted.current.add(provider.id)
    void refreshModels()
  }, [mode, needsDiscovery, provider.id, refreshModels])
  const loadingModels = isPending
  const model = options[0]?.models.find(
    (candidate) => candidate.id === selection.modelId
  )
  const thinking = model?.tool_thinking_options ?? model?.thinking_options ?? []
  return (
    <div className="space-y-5">
      {mode === 'connection' &&
        (provider.workspace_ready && !editing ? (
          <div className="flex flex-wrap items-center justify-between gap-3 rounded-lg border p-3">
            <div className="text-base sm:text-sm">
              <p className="font-medium">
                Use saved {provider.name} connection
              </p>
              <p className="text-muted-foreground">
                No need to connect again. Saved connections are shared across
                this Temps instance.
              </p>
            </div>
            <Button
              type="button"
              size="sm"
              variant="outline"
              onClick={() => setEditing(true)}
            >
              Manage connection
            </Button>
          </div>
        ) : (
          <ProviderEditor
            provider={provider}
            isActive={false}
            embedded
            onVerificationPending={onVerificationPending}
          />
        ))}
      {mode === 'connection' && !provider.workspace_ready && (
        <p role="status" className="text-base sm:text-sm text-muted-foreground">
          {provider.workspace_readiness_hint ??
            'Save a workspace-compatible credential to continue.'}
        </p>
      )}
      {mode === 'model' && provider.workspace_ready && (
        <div className="grid gap-4 sm:grid-cols-2">
          <div className="space-y-2">
            <Label htmlFor="workspace-model">Model</Label>
            <SearchableSelect
              icon={
                <AiHarnessLogo
                  providerId={provider.id}
                  size={16}
                  className="mr-2 shrink-0"
                />
              }
              disabled={loadingModels || !options[0]?.models.length}
              value={selection.modelId ?? undefined}
              options={(options[0]?.models ?? []).map((item) => ({
                value: item.id,
                label: item.name,
                icon: <AiHarnessLogo providerId={provider.id} size={16} />,
                keywords: item.id,
              }))}
              onValueChange={(modelId) =>
                onChange(
                  resolveChatRuntimeSelection(options, provider.id, {
                    modelId,
                    permissionModeId: selection.permissionModeId,
                  })
                )
              }
              placeholder={
                loadingModels
                  ? 'Loading models…'
                  : options[0]?.models.length
                    ? 'Choose model'
                    : 'Models unavailable'
              }
              searchPlaceholder="Search models…"
              title="Choose model"
              className="w-full"
            />
            {loadingModels && (
              <p role="status" className="text-sm text-muted-foreground">
                Discovering models for your saved connection…
              </p>
            )}
            {modelError && (
              <div className="space-y-2">
                <p role="alert" className="text-sm text-destructive">
                  {modelError}
                </p>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  disabled={isPending}
                  onClick={() => void refreshModels()}
                >
                  Retry loading models
                </Button>
              </div>
            )}
            <p className="text-base sm:text-sm text-muted-foreground">
              {provider.model_source === 'bootstrap' ||
              provider.model_source === 'stale_cache'
                ? 'Model availability is not yet verified for this connection.'
                : 'Used for your first task. You can change it later in chat.'}
            </p>
          </div>
          <div className="space-y-2">
            <Label htmlFor="workspace-thinking">Thinking</Label>
            {thinking.length > 0 ? (
              <Select
                value={selection.thinkingOptionId ?? undefined}
                onValueChange={(thinkingOptionId) =>
                  onChange({ ...selection, thinkingOptionId })
                }
              >
                <SelectTrigger id="workspace-thinking">
                  <SelectValue placeholder="Harness default" />
                </SelectTrigger>
                <SelectContent>
                  {thinking.map((item) => (
                    <SelectItem key={item.id} value={item.id}>
                      {item.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            ) : (
              <p className="text-base sm:text-sm text-muted-foreground">
                {loadingModels
                  ? 'Loading thinking options…'
                  : !model
                    ? 'Choose a model to see thinking options.'
                    : 'This model manages thinking automatically.'}
              </p>
            )}
          </div>
          <div className="space-y-2">
            <Label htmlFor="workspace-permissions">Tool permissions</Label>
            <Select
              value={selection.permissionModeId ?? undefined}
              onValueChange={(permissionModeId) =>
                onChange({ ...selection, permissionModeId })
              }
            >
              <SelectTrigger id="workspace-permissions">
                <SelectValue placeholder="Harness default" />
              </SelectTrigger>
              <SelectContent>
                {provider.permission_modes.map((item) => (
                  <SelectItem key={item.id} value={item.id}>
                    {item.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <p className="text-base sm:text-sm text-muted-foreground">
              Auto allows tools without per-command approval. Use it only for
              tasks and code you trust.
            </p>
          </div>
        </div>
      )}
    </div>
  )
}
