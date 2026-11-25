// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  deletePrivateIngress,
  getPrivateIngress,
  putPrivateIngress,
} from '../../api/sdk.gen.js'
import type {
  PrivateIngressRequest,
  PrivateIngressStatus,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { newline, header, icons, json, colors, success, info, keyValue, warning } from '../../ui/output.js'

export interface PrivateOptions {
  listen?: string
  admit?: string[]
  tailscaleServe?: boolean
  off?: boolean
  json?: boolean
}

/**
 * What `ingress private` was asked to do: show the status, withdraw the
 * consent, or store one. `--off` with `--listen` is a contradiction.
 */
export type PrivateIntent =
  | { kind: 'status' }
  | { kind: 'off' }
  | { kind: 'serve'; request: PrivateIngressRequest }
  | { kind: 'invalid'; error: string }

export function privateIntent(options: PrivateOptions): PrivateIntent {
  if (options.off) {
    if (options.listen !== undefined || options.tailscaleServe || options.admit?.length) {
      return {
        kind: 'invalid',
        error: '--off cannot be combined with --listen, --admit or --tailscale-serve',
      }
    }
    return { kind: 'off' }
  }
  if (options.listen === undefined) {
    if (options.tailscaleServe || options.admit?.length) {
      return {
        kind: 'invalid',
        error: '--admit and --tailscale-serve need --listen (one of this machine\'s private addresses, or tailscale)',
      }
    }
    return { kind: 'status' }
  }
  const listen = options.listen.trim()
  if (listen === '') {
    return { kind: 'invalid', error: '--listen needs one of this machine\'s private addresses, or "tailscale"' }
  }
  const admit = (options.admit ?? []).map((range) => range.trim()).filter(Boolean)
  return {
    kind: 'serve',
    request: { listen, admit, tailscale_serve: options.tailscaleServe ?? false },
  }
}

/** One line saying whether this machine serves private addresses, and if not why. */
export function describePrivateStatus(
  status: Pick<PrivateIngressStatus, 'consent'> &
    Partial<Pick<PrivateIngressStatus, 'serving' | 'error' | 'admit'>>,
): string {
  if (!status.consent) {
    return 'This machine does not serve private addresses.'
  }
  if (status.serving) {
    const admit = status.admit?.length ? `, to ${status.admit.join(', ')} only` : ''
    return `Serving private addresses on ${status.serving}${admit}.`
  }
  if (status.error) {
    return `Not serving: ${status.error}`
  }
  return 'Waiting for this machine to apply it.'
}

type NetworkOption = NonNullable<PrivateIngressStatus['networks']>[number]

export function networkName(network: NetworkOption): string {
  return network.tailscale ? `Tailscale (${network.address})` : `${network.interface} (${network.address})`
}

/** What serving on a network would admit, or why it cannot be used, and the flag to choose it. */
export function describeNetwork(network: NetworkOption): string {
  if (network.problem) return `Cannot be used: ${network.problem}`
  const listen = network.tailscale ? 'tailscale' : network.address
  return `admits ${network.admit.join(', ')} · --listen ${listen}`
}

export function registerIngressCommands(program: Command): void {
  const ingress = program
    .command('ingress')
    .description('How this Temps machine receives traffic for your apps')

  ingress
    .command('private')
    .description(
      'Serve private addresses from this machine, only to connections from your private network (Tailscale, WireGuard, NetBird, your LAN…)',
    )
    .option('--listen <address>', 'One of this machine\'s private addresses, or "tailscale" for its Tailscale address')
    .option(
      '--admit <cidr...>',
      'Who may connect, as private ranges (default: the network of the --listen address; Tailscale\'s ranges on Tailscale)',
    )
    .option('--tailscale-serve', 'Also let Temps write its entries in Tailscale Serve')
    .option('--off', 'Stop serving private addresses from this machine')
    .option('--json', 'Output in JSON format')
    .action(privateAction)
}

async function privateAction(options: PrivateOptions): Promise<void> {
  const intent = privateIntent(options)
  if (intent.kind === 'invalid') {
    warning(intent.error)
    process.exitCode = 1
    return
  }

  await requireAuth()
  await setupClient()

  if (intent.kind === 'off') {
    await withSpinner('Withdrawing consent...', async () => {
      const { error } = await deletePrivateIngress({ client })
      if (error) throw new Error(getErrorMessage(error))
    })
    success('This machine no longer serves private addresses')
    return
  }

  if (intent.kind === 'serve') {
    await withSpinner('Storing consent...', async () => {
      const { error } = await putPrivateIngress({ client, body: intent.request })
      if (error) throw new Error(getErrorMessage(error))
    })
    const who = intent.request.admit?.length
      ? intent.request.admit.join(', ')
      : 'its own network'
    success(`This machine will serve private addresses on ${intent.request.listen}, to ${who} only`)
    info('It applies within a few seconds. Run `ingress private` to see the address it serves on.')
    return
  }

  const status = await withSpinner('Fetching status...', async () => {
    const { data, error } = await getPrivateIngress({ client })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })
  if (!status) {
    warning('No status returned')
    return
  }
  if (options.json) {
    json(status)
    return
  }
  newline()
  header(`${icons.info} Private addresses on this machine`)
  keyValue('Listen', status.consent?.listen ?? colors.muted('Not set'))
  keyValue('Tailscale Serve', status.consent ? (status.consent.tailscale_serve ? 'Yes' : 'No') : colors.muted('-'))
  keyValue('Status', describePrivateStatus(status))
  if (status.networks?.length) {
    newline()
    header('Private networks on this machine')
    for (const network of status.networks) {
      keyValue(networkName(network), describeNetwork(network))
    }
  }
  newline()
}
