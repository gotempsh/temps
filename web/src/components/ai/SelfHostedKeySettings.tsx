// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Loader2, ShieldCheck } from 'lucide-react'
import {
  type ProviderKeyResponse,
  type UpdateProviderKeyRequest,
} from '@/api/client'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Label } from '@/components/ui/label'
import { isSelfHostedProvider } from '@/lib/ai-providers'
import {
  SELF_HOSTED_CAPABILITY_LABELS,
  hasIpLiteralHost,
  normalizeSelfHostedCapabilities,
  type SelfHostedCapabilitiesInput,
} from './self-hosted-provider'

interface SelfHostedKeySettingsProps {
  providerKey: ProviderKeyResponse
  /** Whether the viewer may trust a private address (settings:write). */
  canTrustPrivateAddress: boolean
  /** Apply a change to this key. The caller owns the request state. */
  onUpdate: (body: UpdateProviderKeyRequest) => void
  /** Whether an update to this key is in flight. */
  pending: boolean
}

/**
 * Per-key settings shown in a provider key's detail row: where it connects,
 * the private address it trusts for itself (#1411) and, for a self-hosted
 * server, the capabilities it declares. Renders nothing for a vendor key
 * that has no per-key trust. Presentational: the page owns the update
 * request, so collapsing a key row never discards it.
 */
export function SelfHostedKeySettings({
  providerKey,
  canTrustPrivateAddress,
  onUpdate,
  pending,
}: SelfHostedKeySettingsProps) {
  const selfHosted = isSelfHostedProvider(providerKey.provider)
  const trusted = providerKey.trusted_private_address ?? null
  const capabilities: SelfHostedCapabilitiesInput =
    normalizeSelfHostedCapabilities(providerKey.capabilities)

  if (!selfHosted && !trusted) return null

  const canTrustThisUrl =
    canTrustPrivateAddress && hasIpLiteralHost(providerKey.base_url ?? '')

  return (
    <div className="mb-3 grid gap-3 rounded-md border bg-background p-3 text-sm">
      {providerKey.base_url && (
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-muted-foreground">Base URL</span>
          <code className="truncate text-xs">{providerKey.base_url}</code>
        </div>
      )}

      <div className="flex flex-wrap items-center gap-2">
        <span className="text-muted-foreground">Private address trust</span>
        {trusted ? (
          <>
            <Badge
              variant="secondary"
              className="gap-1"
              title="This key may reach this exact address even though it is not in Trusted private networks"
            >
              <ShieldCheck className="h-3 w-3" />
              Trusts {trusted}
            </Badge>
            {providerKey.trusted_private_address_set_at && (
              <span className="text-xs text-muted-foreground">
                since{' '}
                {new Date(
                  providerKey.trusted_private_address_set_at
                ).toLocaleString()}
              </span>
            )}
            <Button
              variant="ghost"
              size="sm"
              className="h-7"
              disabled={pending}
              onClick={() => onUpdate({ trust_private_address: false })}
            >
              Remove trust
            </Button>
          </>
        ) : (
          <>
            <span className="text-xs text-muted-foreground">
              None — the address must be in Trusted private networks.
            </span>
            {canTrustThisUrl && (
              <Button
                variant="outline"
                size="sm"
                className="h-7"
                disabled={pending}
                onClick={() => onUpdate({ trust_private_address: true })}
              >
                Trust this address for this key
              </Button>
            )}
          </>
        )}
        {pending && (
          <Loader2 className="h-3.5 w-3.5 animate-spin text-muted-foreground" />
        )}
      </div>

      {selfHosted && (
        <div className="grid gap-2">
          <span className="text-muted-foreground">Declared capabilities</span>
          <div className="flex flex-wrap gap-x-5 gap-y-2">
            {(
              Object.keys(SELF_HOSTED_CAPABILITY_LABELS) as Array<
                keyof SelfHostedCapabilitiesInput
              >
            ).map((capability) => (
              <div key={capability} className="flex items-center gap-2">
                <Checkbox
                  id={`key-${providerKey.id}-${capability}`}
                  checked={capabilities[capability]}
                  disabled={pending}
                  onCheckedChange={(checked) =>
                    onUpdate({
                      capabilities: {
                        ...capabilities,
                        [capability]: checked === true,
                      },
                    })
                  }
                />
                <Label
                  htmlFor={`key-${providerKey.id}-${capability}`}
                  className="text-sm font-normal"
                  title={SELF_HOSTED_CAPABILITY_LABELS[capability].description}
                >
                  {SELF_HOSTED_CAPABILITY_LABELS[capability].label}
                </Label>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  )
}
