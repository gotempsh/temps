// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Ingress tunnels (ADR 058): tunnels that bring visitors to apps on workers
// with no open ports or certificates, and which hostnames each publishes.
// Calls the `/ingress/*` endpoints through the generated SDK.

import type { Command } from 'commander'
import {
  adminListNodes,
  createIngressConnector,
  getIngressConnectorLinks,
  getIngressHostname,
  listIngressConnectors,
  listIngressHostnames,
  setIngressConnectorNodes,
  setIngressHostnameChoice,
  syncIngressConnector,
  updateIngressConnector,
} from '../../api/sdk.gen.js'
import type { ConnectorView } from '../../api/types.gen.js'
import { requireAuth } from '../../config/store.js'
import { setupClient, getErrorMessage } from '../../lib/api-client.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable } from '../../ui/table.js'
import { colors, header, info, json, keyValue, newline, success } from '../../ui/output.js'

/** The body of a generated SDK call, or its error as a thrown message. */
async function unwrap<T>(
  call: Promise<{ data?: T; error?: unknown }>
): Promise<T> {
  const { data, error } = await call
  if (error || data === undefined) throw new Error(getErrorMessage(error))
  return data
}

async function ready(): Promise<void> {
  await requireAuth()
  await setupClient()
}

function parseIdArg(value: string, what: string): number {
  const id = Number(value)
  if (!Number.isInteger(id) || id <= 0) throw new Error(`Invalid ${what}: ${value}`)
  return id
}

/** One line per worker, for the tunnels table. */
export function describeWorkers(connector: ConnectorView): string {
  if (connector.nodes.length === 0) return 'none'
  return connector.nodes
    .map((n) =>
      n.state === 'ready'
        ? `${n.node_name} (connected${n.ready_connections != null ? `, ${n.ready_connections} links` : ''})`
        : `${n.node_name} (${n.state})`
    )
    .join(', ')
}

function stateColor(state: string): string {
  switch (state) {
    case 'published':
    case 'ready':
      return colors.success(state)
    case 'error':
      return colors.error(state)
    default:
      return colors.warning(state)
  }
}

export function registerTunnelsCommands(program: Command): void {
  const tunnels = program
    .command('tunnels')
    .description(
      'Ingress tunnels: publish apps on workers with no open ports or certificates. ' +
        'Apps under a tunnel\'s domain go live by adding the hostname to an environment'
    )

  tunnels
    .command('list')
    .description('List tunnels, what each publishes and the workers it runs on')
    .option('--page <page>', 'Page number (1-indexed)', '1')
    .option('--page-size <pageSize>', 'Items per page (max 100)', '20')
    .option('--json', 'Output in JSON format')
    .action(async (options: { page?: string; pageSize?: string; json?: boolean }) => {
      await ready()
      const page = Number.parseInt(options.page ?? '1', 10) || 1
      const pageSize = Number.parseInt(options.pageSize ?? '20', 10) || 20
      const list = await withSpinner('Loading tunnels...', () =>
        unwrap(listIngressConnectors({ query: { page, page_size: pageSize } }))
      )
      if (options.json) return json(list)
      printTable(list.connectors, [
        { header: 'ID', key: 'id' },
        { header: 'Name', key: 'name' },
        { header: 'Domain', accessor: (c) => `*.${c.domain}` },
        { header: 'Status', accessor: (c) => c.status, color: (v) => stateColor(v) },
        {
          header: 'Publishes',
          accessor: (c) => (c.auto_publish ? 'every app' : 'only chosen'),
        },
        { header: 'Workers', accessor: describeWorkers },
      ])
      if (list.total > page * pageSize) {
        newline()
        info(`Run with --page ${page + 1} to see more`)
      }
    })

  tunnels
    .command('workers')
    .description('List worker nodes, for choosing where a tunnel runs (create --node)')
    .option('--json', 'Output in JSON format')
    .action(async (options: { json?: boolean }) => {
      await ready()
      const { nodes } = await unwrap(adminListNodes())
      const workers = nodes.filter((n) => n.role === 'worker')
      if (options.json) return json(workers)
      printTable(workers, [
        { header: 'ID', key: 'id' },
        { header: 'Name', key: 'name' },
        { header: 'Status', key: 'status' },
      ])
    })

  tunnels
    .command('create')
    .description(
      'Create a tunnel for a managed zone and run it on workers. The zone must already be ' +
        'managed by the DNS provider (dns-providers domains add); workers must allow ingress'
    )
    .requiredOption('--name <name>', 'Lowercase letters, digits and hyphens')
    .requiredOption('--dns-provider <id>', 'Cloudflare DNS provider id')
    .requiredOption('--domain <domain>', 'Zone (or subdomain of one) whose apps it publishes')
    .requiredOption('--node <ids...>', 'Worker node ids to run it on')
    .option('--provider <kind>', 'Tunnel provider kind', 'cloudflare_tunnel')
    .option('--json', 'Output in JSON format')
    .action(
      async (options: {
        name: string
        dnsProvider: string
        domain: string
        node: string[]
        provider: string
        json?: boolean
      }) => {
        await ready()
        const connector = await withSpinner('Creating the tunnel...', () =>
          unwrap(
            createIngressConnector({
              body: {
                name: options.name,
                provider: options.provider,
                values: {
                  dns_provider_id: parseIdArg(options.dnsProvider, 'DNS provider id'),
                  domain: options.domain,
                },
                node_ids: options.node.map((n) => parseIdArg(n, 'node id')),
              },
            })
          )
        )
        if (options.json) return json(connector)
        success(
          `Tunnel ${connector.name} created for *.${connector.domain}. Workers connect within seconds.`
        )
      }
    )

  tunnels
    .command('hostnames')
    .description('List hostnames tunnels publish or could publish, with their state and why')
    .option('--json', 'Output in JSON format')
    .action(async (options: { json?: boolean }) => {
      await ready()
      const hosts = await withSpinner('Loading hostnames...', () =>
        unwrap(listIngressHostnames())
      )
      if (options.json) return json(hosts)
      printTable(hosts, [
        { header: 'Hostname', key: 'hostname' },
        { header: 'State', accessor: (h) => h.state, color: (v) => stateColor(v) },
        { header: 'Tunnel', accessor: (h) => h.connector_name ?? '' },
        { header: 'Mode', key: 'mode' },
        { header: 'Detail', key: 'detail' },
      ])
    })

  tunnels
    .command('explain <hostname>')
    .description('Explain how a hostname reaches visitors: which tunnel carries it, or why none does')
    .option('--json', 'Output in JSON format')
    .action(async (hostname: string, options: { json?: boolean }) => {
      await ready()
      const view = await unwrap(getIngressHostname({ path: { hostname } }))
      if (options.json) return json(view)
      header(view.hostname)
      keyValue('State', view.state)
      keyValue('Mode', view.mode)
      keyValue('Tunnel', view.connector_name ?? null)
      keyValue('Served by workers', view.served_by_workers)
      keyValue('Detail', view.detail)
      keyValue(
        'Tunnels covering it',
        view.candidates.map((c) => `${c.name} (#${c.connector_id})`).join(', ') || 'none'
      )
      newline()
    })

  tunnels
    .command('route <hostname>')
    .description(
      'Choose how a hostname reaches visitors: through a tunnel (--tunnel <id>), whichever ' +
        'tunnel covers it (--automatic), or no tunnel (--off)'
    )
    .option('--tunnel <id>', 'Pin it to this tunnel')
    .option('--automatic', 'Let the most specific covering tunnel publish it')
    .option('--off', 'Keep every tunnel away from it (DNS managed elsewhere)')
    .option('--json', 'Output in JSON format')
    .action(
      async (
        hostname: string,
        options: { tunnel?: string; automatic?: boolean; off?: boolean; json?: boolean }
      ) => {
        const picked = [options.tunnel !== undefined, !!options.automatic, !!options.off]
        if (picked.filter(Boolean).length !== 1) {
          throw new Error('Pass exactly one of --tunnel <id>, --automatic or --off')
        }
        await ready()
        const view = await unwrap(
          setIngressHostnameChoice({
            path: { hostname },
            body: options.tunnel
              ? { mode: 'tunnel', connector_id: parseIdArg(options.tunnel, 'tunnel id') }
              : { mode: options.off ? 'off' : 'automatic' },
          })
        )
        if (options.json) return json(view)
        success(`${view.hostname}: ${view.detail}`)
      }
    )

  tunnels
    .command('publish <id>')
    .description('Set what a tunnel publishes: every app under its domain (--all) or only chosen hostnames (--chosen)')
    .option('--all', 'Publish every app under its domain')
    .option('--chosen', 'Publish only hostnames routed to it')
    .action(async (id: string, options: { all?: boolean; chosen?: boolean }) => {
      if (!!options.all === !!options.chosen) {
        throw new Error('Pass exactly one of --all or --chosen')
      }
      await ready()
      const connector = await unwrap(
        updateIngressConnector({
          path: { id: parseIdArg(id, 'tunnel id') },
          body: { auto_publish: !!options.all },
        })
      )
      success(
        connector.auto_publish
          ? `${connector.name} publishes every app under ${connector.domain}`
          : `${connector.name} publishes only hostnames routed to it`
      )
    })

  tunnels
    .command('nodes <id>')
    .description('Set the workers a tunnel runs on (an empty list stops it everywhere)')
    .requiredOption('--node <ids...>', 'Worker node ids')
    .action(async (id: string, options: { node: string[] }) => {
      await ready()
      const connector = await unwrap(
        setIngressConnectorNodes({
          path: { id: parseIdArg(id, 'tunnel id') },
          body: { node_ids: options.node.map((n) => parseIdArg(n, 'node id')) },
        })
      )
      success(`${connector.name} runs on: ${describeWorkers(connector)}`)
    })

  tunnels
    .command('links <id>')
    .description("Show a tunnel's live connections to the provider: data centre, age, worker")
    .option('--json', 'Output in JSON format')
    .action(async (id: string, options: { json?: boolean }) => {
      await ready()
      const links = await unwrap(
        getIngressConnectorLinks({ path: { id: parseIdArg(id, 'tunnel id') } })
      )
      if (options.json) return json(links)
      keyValue('Provider status', links.status)
      printTable(links.links, [
        { header: 'Worker', accessor: (l) => l.node_name ?? 'unknown' },
        { header: 'Data centre', accessor: (l) => l.location ?? '' },
        { header: 'Opened', accessor: (l) => l.opened_at ?? '' },
        { header: 'Reconnecting', accessor: (l) => (l.reconnecting ? 'yes' : 'no') },
      ])
    })

  tunnels
    .command('sync <id>')
    .description('Re-check a tunnel with its provider and recreate it if it was deleted there')
    .action(async (id: string) => {
      await ready()
      const connector = await withSpinner('Syncing...', () =>
        unwrap(syncIngressConnector({ path: { id: parseIdArg(id, 'tunnel id') } }))
      )
      success(`${connector.name}: ${connector.status}`)
    })
}
