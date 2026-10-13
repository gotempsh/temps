// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMutation } from '@tanstack/react-query'
import { Link } from 'react-router'
import { Loader2, RefreshCw, ShieldAlert } from 'lucide-react'
import { discoverProviderModels } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Label } from '@/components/ui/label'
import { SearchableSelect } from '@/components/ui/searchable-select'
import { problemDetail } from '@/lib/api-problem'
import { SELF_HOSTED_PROVIDER_ID } from '@/lib/ai-providers'
import {
  SELF_HOSTED_CAPABILITY_LABELS,
  hasIpLiteralHost,
  isPlainHttpUrl,
  selfHostedDiscoverySignature,
  type SelfHostedCapabilitiesInput,
} from './self-hosted-provider'

export interface SelfHostedDiscovery {
  /** Inputs the list was discovered with (see `selfHostedDiscoverySignature`). */
  signature: string
  models: string[]
}

interface SelfHostedProviderFieldsProps {
  /**
   * Whether the fields are shown (the self-hosted provider is selected). The
   * component stays mounted either way, so discovery request state survives
   * switching providers back and forth.
   */
  open: boolean
  baseUrl: string
  apiKey: string
  trustPrivateAddress: boolean
  onTrustPrivateAddressChange: (trusted: boolean) => void
  /** Whether the viewer may trust a private address (settings:write). */
  canTrustPrivateAddress: boolean
  discovery: SelfHostedDiscovery | null
  onDiscovered: (discovery: SelfHostedDiscovery | null) => void
  defaultModel: string
  onDefaultModelChange: (model: string) => void
  capabilities: SelfHostedCapabilitiesInput
  onCapabilitiesChange: (capabilities: SelfHostedCapabilitiesInput) => void
  /** Form validation message for the default model, if any. */
  error?: string
  disabled?: boolean
}

/**
 * The parts of the add-provider form that only a self-hosted
 * OpenAI-compatible server needs: listing its models so the administrator
 * picks the default (there is no vendor default to verify with), the
 * capabilities it implements, and trusting its exact private address on this
 * key only.
 */
export function SelfHostedProviderFields({
  open,
  baseUrl,
  apiKey,
  trustPrivateAddress,
  onTrustPrivateAddressChange,
  canTrustPrivateAddress,
  discovery,
  onDiscovered,
  defaultModel,
  onDefaultModelChange,
  capabilities,
  onCapabilitiesChange,
  error,
  disabled = false,
}: SelfHostedProviderFieldsProps) {
  const signature = selfHostedDiscoverySignature({
    baseUrl,
    apiKey,
    trustPrivateAddress,
  })
  const currentModels =
    discovery && discovery.signature === signature ? discovery.models : null

  const discoverMutation = useMutation({
    // The variable is the signature of the inputs the request was made with,
    // so results and errors only apply while those inputs are unchanged.
    mutationFn: async (_requestSignature: string) => {
      const response = await discoverProviderModels({
        body: {
          provider: SELF_HOSTED_PROVIDER_ID,
          api_key: apiKey,
          base_url: baseUrl.trim(),
          trust_private_address: trustPrivateAddress,
        },
        throwOnError: true,
      })
      return response.data
    },
    onSuccess: (data, requestSignature) => {
      const models = data.models.map((model) => model.id)
      onDiscovered({ signature: requestSignature, models })
      if (!models.includes(defaultModel)) {
        onDefaultModelChange(models[0] ?? '')
      }
    },
    onError: () => onDiscovered(null),
  })

  const ipLiteral = hasIpLiteralHost(baseUrl)
  const plainHttp = isPlainHttpUrl(baseUrl)
  const discoveryError =
    discoverMutation.isError && discoverMutation.variables === signature
      ? discoverMutation.error
      : null

  if (!open) return null

  return (
    <div className="grid gap-4">
      <div className="grid gap-2 rounded-md border bg-muted/20 p-3">
        <div className="flex items-start gap-2">
          <Checkbox
            id="trustPrivateAddress"
            checked={trustPrivateAddress}
            disabled={disabled || !canTrustPrivateAddress}
            onCheckedChange={(checked) =>
              onTrustPrivateAddressChange(checked === true)
            }
            className="mt-0.5"
          />
          <div className="grid gap-1">
            <Label htmlFor="trustPrivateAddress" className="font-medium">
              Trust this private address for this key only
            </Label>
            <p className="text-xs text-muted-foreground">
              Lets this key reach the exact address and port in the base URL
              (for example <code>10.0.0.5:8000</code>) without adding it to{' '}
              <Link
                to="/settings/security"
                className="underline hover:text-foreground"
              >
                Trusted private networks
              </Link>
              , which every outbound feature (webhooks, monitors, Git, ...)
              honours. The base URL must use the server&apos;s IP address. Cloud
              metadata and link-local addresses are always blocked.
            </p>
            {!canTrustPrivateAddress && (
              <p className="text-xs text-muted-foreground">
                Only an instance administrator can trust a private address.
              </p>
            )}
            {trustPrivateAddress && baseUrl.trim() && !ipLiteral && (
              <p className="text-xs text-destructive">
                Use the server&apos;s IP address in the base URL to trust it on
                this key, or add it to Trusted private networks instead.
              </p>
            )}
          </div>
        </div>
        {plainHttp && (
          <p className="flex items-start gap-1.5 text-xs text-muted-foreground">
            <ShieldAlert className="mt-0.5 h-3.5 w-3.5 shrink-0" />
            Plain http:// is only accepted for a trusted private address, so a
            key and prompts never cross the internet unencrypted.
          </p>
        )}
      </div>

      <div className="grid gap-2">
        <div className="flex items-center justify-between gap-2">
          <Label htmlFor="selfHostedDefaultModel">Default model</Label>
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={disabled || !baseUrl.trim() || discoverMutation.isPending}
            onClick={() => discoverMutation.mutate(signature)}
          >
            {discoverMutation.isPending ? (
              <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" />
            ) : (
              <RefreshCw className="mr-1.5 h-3.5 w-3.5" />
            )}
            {currentModels ? 'Reload models' : 'Load models'}
          </Button>
        </div>
        <SearchableSelect
          value={currentModels ? defaultModel : undefined}
          onValueChange={onDefaultModelChange}
          options={(currentModels ?? []).map((model) => ({
            value: model,
            label: model,
          }))}
          placeholder={
            currentModels
              ? 'Pick the model to verify with'
              : 'Load the models your server lists first'
          }
          searchPlaceholder="Search models..."
          emptyText="No model matches."
          searchMode="contains"
          disabled={disabled || !currentModels || currentModels.length === 0}
        />
        {discoveryError ? (
          <p role="alert" className="text-xs text-destructive">
            {problemDetail(
              discoveryError,
              'Could not list the models your server serves.'
            )}
          </p>
        ) : currentModels ? (
          <p className="text-xs text-muted-foreground">
            {currentModels.length === 0
              ? 'Your server lists no models at /models. Load a model on it and reload.'
              : `${currentModels.length} model${currentModels.length === 1 ? '' : 's'} found. Every listed model is routed only to this server; the default is used to verify the connection.`}
          </p>
        ) : (
          <p className="text-xs text-muted-foreground">
            Temps asks your server for <code>/models</code> and lists every
            model it serves, with no name filter.
          </p>
        )}
        {error && !discoveryError && (
          <p className="text-xs text-destructive">{error}</p>
        )}
      </div>

      <div className="grid gap-2">
        <Label>What your server supports</Label>
        <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
          {(
            Object.keys(SELF_HOSTED_CAPABILITY_LABELS) as Array<
              keyof SelfHostedCapabilitiesInput
            >
          ).map((capability) => (
            <div key={capability} className="flex items-start gap-2">
              <Checkbox
                id={`capability-${capability}`}
                checked={capabilities[capability]}
                disabled={disabled}
                onCheckedChange={(checked) =>
                  onCapabilitiesChange({
                    ...capabilities,
                    [capability]: checked === true,
                  })
                }
                className="mt-0.5"
              />
              <div className="grid gap-0.5">
                <Label
                  htmlFor={`capability-${capability}`}
                  className="text-sm font-normal"
                >
                  {SELF_HOSTED_CAPABILITY_LABELS[capability].label}
                </Label>
                <span className="text-xs text-muted-foreground">
                  {SELF_HOSTED_CAPABILITY_LABELS[capability].description}
                </span>
              </div>
            </div>
          ))}
        </div>
      </div>
    </div>
  )
}
