// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useRef } from 'react'
import { useMutation } from '@tanstack/react-query'
import { Link } from 'react-router'
import { ChevronDown, Loader2, RefreshCw, ShieldAlert } from 'lucide-react'
import { discoverProviderModels } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import { Label } from '@/components/ui/label'
import { SearchableSelect } from '@/components/ui/searchable-select'
import { useDebounce } from '@/hooks/useDebounce'
import { problemDetail } from '@/lib/api-problem'
import { SELF_HOSTED_PROVIDER_ID } from '@/lib/ai-providers'
import {
  SELF_HOSTED_CAPABILITY_LABELS,
  hasIpLiteralHost,
  isDiscoverableBaseUrl,
  isPlainHttpUrl,
  isPrivateAddressUrl,
  selfHostedDiscoverySignature,
  type SelfHostedCapabilitiesInput,
} from './self-hosted-provider'

export interface SelfHostedDiscovery {
  /** Inputs the list was discovered with (see `selfHostedDiscoverySignature`). */
  signature: string
  /** Model ids, in the order the server listed them. */
  models: string[]
  /** Human-readable names by model id, for the models the server named. */
  names?: Record<string, string>
}

interface SelfHostedProviderFieldsProps {
  /**
   * Whether the fields are shown (the self-hosted provider is selected). The
   * component stays mounted either way, so discovery request state survives
   * switching providers back and forth.
   */
  open: boolean
  baseUrl: string
  /**
   * The base URL as the administrator last finished entering it: the value
   * when they left the field or pasted into it. With an API key set, models
   * load on their own only for this URL, never for one still being typed.
   */
  committedBaseUrl?: string
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

/** How long the inputs must be still before the models are listed. */
const DISCOVERY_DEBOUNCE_MS = 600

/**
 * The parts of the add-provider form that only a self-hosted
 * OpenAI-compatible server needs: listing its models so the administrator
 * picks the default (there is no vendor default to verify with), trusting
 * its exact private address on this key only (offered only when the base URL
 * is private), and, as an advanced option, narrowing the capabilities it
 * declares (a new key declares all of them).
 */
export function SelfHostedProviderFields({
  open,
  baseUrl,
  committedBaseUrl = '',
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
  const modelNames = discovery?.names ?? {}
  // Results of a request made with older inputs must not reset the default
  // model or clear a newer listing.
  const latestSignature = useRef(signature)
  useEffect(() => {
    latestSignature.current = signature
  }, [signature])

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
      if (requestSignature !== latestSignature.current) return
      const models = data.models.map((model) => model.id)
      const names = Object.fromEntries(
        data.models.flatMap((model) => {
          const name = model.name?.trim()
          return name ? [[model.id, name]] : []
        })
      )
      onDiscovered({ signature: requestSignature, models, names })
      if (!models.includes(defaultModel)) {
        onDefaultModelChange(models[0] ?? '')
      }
    },
    onError: (_error, requestSignature) => {
      if (requestSignature === latestSignature.current) onDiscovered(null)
    },
  })

  // Listing the models is how the form checks the server. It runs on its own
  // once the inputs stop changing, at most once per set of inputs, so a
  // failure is shown instead of retried in a loop. With an API key it also
  // waits until the base URL is committed (the field was left or pasted
  // into): a pause while typing the URL leaves a prefix that is often a real,
  // someone-else's host (`https://api.example.co` on the way to `.com`), and
  // discovery sends the key to it as a bearer token.
  const settledSignature = useDebounce(signature, DISCOVERY_DEBOUNCE_MS)
  const { mutate: discover, variables: requestedSignature } = discoverMutation
  const discoverable = isDiscoverableBaseUrl(baseUrl)
  const committed =
    baseUrl.trim() !== '' && committedBaseUrl.trim() === baseUrl.trim()
  const automatic = apiKey === '' || committed
  const listed = currentModels !== null
  useEffect(() => {
    if (!open || disabled || !discoverable || !automatic || listed) return
    if (settledSignature !== signature || requestedSignature === signature) {
      return
    }
    discover(signature)
  }, [
    open,
    disabled,
    discoverable,
    automatic,
    listed,
    settledSignature,
    signature,
    requestedSignature,
    discover,
  ])

  const ipLiteral = hasIpLiteralHost(baseUrl)
  const plainHttp = isPlainHttpUrl(baseUrl)
  // A public endpoint needs no trust. Once ticked it stays visible, so a
  // choice made for an earlier URL can always be taken back.
  const showTrust = isPrivateAddressUrl(baseUrl) || trustPrivateAddress
  const narrowed = Object.values(capabilities).some((supported) => !supported)
  const discoveryError =
    discoverMutation.isError && discoverMutation.variables === signature
      ? discoverMutation.error
      : null
  const checking =
    discoverMutation.isPending && discoverMutation.variables === signature

  if (!open) return null

  return (
    <div className="grid gap-4">
      {showTrust && (
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
                  to="/settings/security#trusted-private-network-settings"
                  className="underline hover:text-foreground"
                >
                  Trusted private networks
                </Link>
                , which every outbound feature (webhooks, monitors, Git, ...)
                honours. The base URL must use the server&apos;s IP address.
                Cloud metadata and link-local addresses are always blocked.
              </p>
              {!canTrustPrivateAddress && (
                <p className="text-xs text-muted-foreground">
                  Only an instance administrator can trust a private address.
                </p>
              )}
              {trustPrivateAddress && baseUrl.trim() && !ipLiteral && (
                <p className="text-xs text-destructive">
                  Use the server&apos;s IP address in the base URL to trust it
                  on this key, or add it to Trusted private networks instead.
                </p>
              )}
            </div>
          </div>
        </div>
      )}
      {plainHttp && (
        <p className="flex items-start gap-1.5 text-xs text-muted-foreground">
          <ShieldAlert className="mt-0.5 h-3.5 w-3.5 shrink-0" />
          Plain http:// is only accepted for a trusted private address, so a key
          and prompts never cross the internet unencrypted.
        </p>
      )}

      <div className="grid gap-2">
        <div className="flex items-center justify-between gap-2">
          <Label htmlFor="selfHostedDefaultModel">Default model</Label>
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={disabled || !discoverable || checking}
            onClick={() => discover(signature)}
          >
            {checking ? (
              <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" />
            ) : (
              <RefreshCw className="mr-1.5 h-3.5 w-3.5" />
            )}
            {currentModels || discoveryError ? 'Reload models' : 'Load models'}
          </Button>
        </div>
        <SearchableSelect
          value={currentModels ? defaultModel : undefined}
          onValueChange={onDefaultModelChange}
          options={(currentModels ?? []).map((model) => ({
            value: model,
            // The server's name for the model; its id stays searchable.
            label: modelNames[model] ?? model,
            keywords: model,
          }))}
          placeholder={
            currentModels
              ? 'Pick the model to verify with'
              : checking
                ? 'Loading the models your server lists...'
                : !discoverable
                  ? 'Enter the base URL to load its models'
                  : automatic
                    ? 'Models load from your server'
                    : 'Models load once you finish the base URL'
          }
          searchPlaceholder="Search models..."
          emptyText="No model matches."
          searchMode="contains"
          disabled={disabled || !currentModels || currentModels.length === 0}
        />
        {checking ? (
          <p role="status" className="text-xs text-muted-foreground">
            Checking your server and listing its models...
          </p>
        ) : discoveryError ? (
          <p role="alert" className="text-xs text-destructive">
            {problemDetail(
              discoveryError,
              'Could not list the models your server serves.'
            )}{' '}
            {apiKey === ''
              ? 'Check the base URL; the models reload as you edit it.'
              : 'Check the base URL and API key; the models reload as you edit them.'}
          </p>
        ) : currentModels ? (
          <p
            className={
              currentModels.length === 0
                ? 'text-xs text-destructive'
                : 'text-xs text-muted-foreground'
            }
          >
            {currentModels.length === 0
              ? 'Your server answered but lists no models at /models. Load a model on it and reload.'
              : `Connected: ${currentModels.length} model${currentModels.length === 1 ? '' : 's'} found. Every listed model is routed only to this server; the default is used to verify the connection.`}
          </p>
        ) : (
          <p className="text-xs text-muted-foreground">
            Temps asks your server for <code>/models</code> and lists every
            model it serves, with no name filter.
            {automatic
              ? null
              : ' The API key is only sent once you leave the base URL field.'}
          </p>
        )}
        {error && !discoveryError && !checking && (
          <p className="text-xs text-destructive">{error}</p>
        )}
      </div>

      <Collapsible defaultOpen={narrowed} className="grid gap-2">
        <CollapsibleTrigger asChild>
          <button
            type="button"
            className="group flex w-fit items-center gap-1 rounded-md text-sm font-medium text-muted-foreground hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            Advanced: what your server supports
            <ChevronDown className="h-4 w-4 transition-transform group-data-[state=open]:rotate-180" />
          </button>
        </CollapsibleTrigger>
        <CollapsibleContent className="grid gap-2">
          <p className="text-xs text-muted-foreground">
            Temps forwards every kind of request to your server by default.
            Untick what it does not implement, and Temps refuses those requests
            with a clear error instead of passing them on.
          </p>
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
        </CollapsibleContent>
      </Collapsible>
    </div>
  )
}
