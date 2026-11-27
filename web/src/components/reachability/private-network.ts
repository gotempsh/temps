// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { PrivateNetworkOption } from '@/api/client'

/** What the consent's `listen` is for a network: Tailscale by name, so a new address is followed. */
export function listenFor(network: PrivateNetworkOption): string {
  return network.tailscale ? 'tailscale' : network.address
}

/** How a network is named to the operator. */
export function networkLabel(network: PrivateNetworkOption): string {
  return network.tailscale
    ? `Tailscale (${network.address})`
    : `${network.interface} (${network.address})`
}
