// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// When a zone is imported from a provider that also runs tunnels, how
// visitors reach the apps under it: DNS pointed at a server, or a tunnel on
// chosen workers (ADR 058 D10). The tunnel is created right after the zone.

import { useQuery } from '@tanstack/react-query'
import {
  adminListNodesOptions,
  listIngressProvidersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Checkbox } from '@/components/ui/checkbox'
import { Label } from '@/components/ui/label'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'

/** The tunnel provider each DNS provider type can create. */
const TUNNEL_FOR_DNS: Record<string, string> = {
  cloudflare: 'cloudflare_tunnel',
}

export type ZoneTunnel = {
  mode: 'direct' | 'tunnel'
  nodeIds: number[]
}

export const NO_ZONE_TUNNEL: ZoneTunnel = { mode: 'direct', nodeIds: [] }

/** The tunnel provider kind for a DNS provider type, if it has one. */
export function tunnelKindFor(providerType: string): string | undefined {
  return TUNNEL_FOR_DNS[providerType.toLowerCase()]
}

/** `autoship.so` → `autoship-so`: a valid tunnel name. */
export function tunnelNameFor(zone: string): string {
  return zone
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 40)
}

export function ZoneTunnelChoice({
  providerType,
  value,
  onChange,
}: {
  providerType: string
  value: ZoneTunnel
  onChange: (value: ZoneTunnel) => void
}) {
  const kind = tunnelKindFor(providerType)
  // Both need settings access; without it the choice is not offered.
  const providersQuery = useQuery({
    ...listIngressProvidersOptions(),
    enabled: !!kind,
    retry: false,
  })
  const nodesQuery = useQuery({
    ...adminListNodesOptions(),
    enabled: !!kind,
    retry: false,
  })
  const provider = providersQuery.data?.find((p) => p.kind === kind)
  const workers = (nodesQuery.data?.nodes ?? []).filter(
    (n) => n.role === 'worker'
  )
  if (!kind || !provider || workers.length === 0) return null

  const toggle = (id: number, on: boolean) =>
    onChange({
      ...value,
      nodeIds: on
        ? [...value.nodeIds, id]
        : value.nodeIds.filter((n) => n !== id),
    })

  return (
    <div className="space-y-3 rounded-lg border p-4">
      <div className="space-y-0.5">
        <p className="text-base font-medium">How visitors reach apps here</p>
        <p className="text-sm text-muted-foreground">
          Apps under this zone can go through {provider.name}: Temps creates
          the tunnel and each app&apos;s DNS record, and Cloudflare serves
          HTTPS. Workers need no public address or open port.
        </p>
      </div>
      <RadioGroup
        value={value.mode}
        onValueChange={(mode) =>
          onChange({ ...value, mode: mode as ZoneTunnel['mode'] })
        }
        className="grid gap-2"
      >
        <label className="flex cursor-pointer items-start gap-2 text-sm">
          <RadioGroupItem value="tunnel" className="mt-0.5" />
          <span>
            Through {provider.name}{' '}
            <span className="text-muted-foreground">(no certificates)</span>
          </span>
        </label>
        <label className="flex cursor-pointer items-start gap-2 text-sm">
          <RadioGroupItem value="direct" className="mt-0.5" />
          <span>
            Point DNS at a server{' '}
            <span className="text-muted-foreground">
              (certificates from Let&apos;s Encrypt)
            </span>
          </span>
        </label>
      </RadioGroup>
      {value.mode === 'tunnel' && (
        <div className="space-y-1.5 pl-6">
          <p className="text-sm text-muted-foreground">Run the tunnel on</p>
          {workers.map((node) => (
            <div key={node.id} className="flex items-center gap-2">
              <Checkbox
                id={`zone-tunnel-node-${node.id}`}
                checked={value.nodeIds.includes(node.id)}
                onCheckedChange={(on) => toggle(node.id, on === true)}
              />
              <Label
                htmlFor={`zone-tunnel-node-${node.id}`}
                className="font-normal"
              >
                {node.name}
                <span className="ml-1 text-muted-foreground">
                  · {node.status}
                </span>
              </Label>
            </div>
          ))}
          <p className="text-xs text-muted-foreground">
            More than one worker keeps apps reachable when one goes down. A
            worker must allow ingress (
            <code>temps agent ingress delegated</code>).
          </p>
        </div>
      )}
    </div>
  )
}
