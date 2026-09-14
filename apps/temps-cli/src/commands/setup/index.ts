// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { createClient } from '../../api/client/index.js'
import { getCurrentUser } from '../../api/sdk.gen.js'
import { getContext, upsertContext } from '../../config/contexts.js'
import { promptConfirm, promptText } from '../../ui/prompts.js'
import { provision, runSsh, validateOptions, SetupError, type SetupOptions, type SetupResult } from './provision.js'
import { setupTelemetry } from './telemetry.js'

export async function verifySetup(result: SetupResult, transport: typeof fetch = fetch): Promise<void> {
  // Fresh client: ambient credentials/context and debug logging cannot leak the key.
  const client = createClient({ baseUrl: `${result.url}/api`, fetch: transport })
  for (let attempt = 0; attempt < 6; attempt++) {
    try {
      const { data, response } = await getCurrentUser({
        client, headers: { Authorization: `Bearer ${result.apiKey}` },
        redirect: 'error', signal: AbortSignal.timeout(5000),
      })
        if (response?.status === 401 || response?.status === 403) {
        throw new SetupError('verify', 'auth_failed', 'The installer API key was rejected. Renew it on the server before retrying; no context was saved.')
      }
      if (data?.email === result.email) return
    } catch (error) {
      if (error instanceof SetupError) throw error
    }
    if (attempt < 5) await new Promise(resolve => setTimeout(resolve, 2000))
  }
  throw new SetupError('verify', 'unreachable', `Could not verify HTTPS and authentication at ${result.url}. Check DNS, certificates and inbound port 443. Installation remains on the server; rerun setup after fixing access.`)
}

interface CommandOptions extends SetupOptions { yes?: boolean; dryRun?: boolean; telemetry?: boolean }

export function registerSetupCommand(program: Command): void {
  program.command('setup')
    .description('PoC: install Temps on an existing Linux VPS over SSH and save a client context')
    .requiredOption('--ssh <destination>', 'SSH alias or user@hostname (trusted host key required)')
    .option('--email <email>', 'Admin email and certificate contact')
    .option('--context <name>', 'New local context name', 'production')
    .option('--port <port>', 'SSH port', '22')
    .option('--identity <path>', 'SSH private key path (otherwise use your SSH agent/config)')
    .option('--channel <channel>', 'Runtime release channel: stable, beta, nightly', 'stable')
    .option('--runtime-version <tag>', 'Pin the runtime release')
    .option('--dry-run', 'Show the plan without connecting, changing files or sending telemetry')
    .option('--telemetry', 'Opt in to coarse setup-step analytics for this attempt only')
    .option('--no-telemetry', 'Do not send setup analytics (default)')
    .option('-y, --yes', 'Approve the installation plan without prompting')
    .addHelpText('after', '\nRequires a Linux VPS, key-based SSH, a trusted host key, root or passwordless sudo, curl and flock.\nQuickStart needs public inbound ports 80/443. Installer logs remain on the server and may contain secrets.\nRuntime telemetry is disabled for fresh installs in this PoC; --telemetry covers CLI setup only.\n')
    .action(async (options: CommandOptions) => {
      if (!options.email) {
        if (!process.stdin.isTTY) throw new SetupError('preflight', 'missing_email', 'Provide --email for non-interactive setup.')
        options.email = await promptText({ message: 'Admin and certificate contact email', required: true })
      }
      validateOptions(options)
      const existing = await getContext(options.context)
      console.log(`Install Temps QuickStart on ${options.ssh} using ${options.runtimeVersion ?? options.channel}; context: ${options.context}.`)
      console.log('The installer may install Docker, PostgreSQL and system services. It uses a public sslip.io hostname. A completed setup is reused on retry.')
      if (existing) console.log(`Existing context ${options.context} will only be refreshed if its URL matches the verified server.`)
      console.log(`CLI setup analytics: ${options.telemetry === true ? 'enabled (random attempt ID, step, status, duration bucket, CLI version)' : 'disabled'}.`)
      if (options.dryRun) return
      if (!options.yes) {
        if (!process.stdin.isTTY) throw new SetupError('preflight', 'approval_required', 'Review --dry-run, then use --yes for non-interactive installation.')
        if (!await promptConfirm({ message: 'Install Temps on this server?', default: false })) return
      }
      const telemetry = setupTelemetry(options.telemetry === true, program.version() ?? 'unknown')
      const heartbeat = setInterval(() => process.stderr.write('Setup is still running; detailed installer logs stay on the server.\n'), 30_000)
      try {
        const result = await provision(options, {
          remote: (script, step) => runSsh(options, script, step),
          verify: result => verifySetup(result),
          save: async result => {
            const current = await getContext(options.context)
            if (current && current.url.replace(/\/api\/?$/, '').replace(/\/$/, '') !== result.url) {
              throw new SetupError('context', 'context_conflict', `Context ${options.context} points to another server. Rerun with a different --context; existing credentials were preserved.`)
            }
            await upsertContext({ name: options.context, url: result.url, apiKey: result.apiKey, email: result.email, isActive: current?.isActive }, { makeActive: false })
          },
          event: (step, status) => {
            process.stderr.write(`${step}: ${status}\n`)
            telemetry.record(step, status)
          },
        })
        console.log(`Temps is ready: ${result.url}`)
        console.log(`Authenticated context saved. Deploy local source: temps --target-context ${options.context} drop .`)
        console.log('Dashboard admin credentials remain in /root/.temps/setup-result.json on the server.')
      } finally {
        clearInterval(heartbeat)
        await telemetry.flush()
      }
    })
}
