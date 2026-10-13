// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import {
  getProjectBySlug,
  getChatReadiness,
  startAiProviderLogin,
  getAiProviderLogin,
  submitAiProviderLoginCode,
  cancelAiProviderLogin,
} from '../../api/sdk.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { promptPassword } from '../../ui/prompts.js'
import {
  LoginFlowError,
  runProviderLogin,
  type LoginSession,
  type ProviderLoginApi,
} from './provider-login.js'
import {
  newline,
  header,
  icons,
  colors,
  keyValue,
  json as jsonOutput,
} from '../../ui/output.js'

interface ReadinessOptions {
  project?: string
  json?: boolean
}

/**
 * Report which AI prerequisites a project satisfies, and — crucially — what to
 * do about the ones it doesn't. Scripting against the AI chat endpoints
 * otherwise means discovering the gates by getting a 409 back mid-run.
 */
async function readiness(
  projectArg: string | undefined,
  options: ReadinessOptions
): Promise<void> {
  await requireAuth()
  await setupClient()

  const resolved = await requireProjectSlug(projectArg ?? options.project)

  const project = await withSpinner('Resolving project...', async () => {
    const { data, error } = await getProjectBySlug({
      client,
      path: { slug: resolved.slug },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    if (!data) {
      throw new Error(`Project "${resolved.slug}" not found`)
    }
    return data
  })

  const status = await withSpinner('Checking AI readiness...', async () => {
    const { data, error } = await getChatReadiness({
      client,
      path: { project_id: project.id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    if (!data) {
      throw new Error(`AI readiness returned no data for "${resolved.slug}"`)
    }
    return data
  })

  if (options.json) {
    jsonOutput(status)
    return
  }

  const mark = (ok: boolean) => (ok ? colors.success(icons.success) : colors.error(icons.error))

  newline()
  header(`AI readiness · ${project.name}`)
  newline()
  keyValue('AI provider configured', mark(status.ai_configured))
  newline()

  const fixes: string[] = []
  if (!status.ai_configured) {
    fixes.push(
      'Add an AI provider key in the dashboard under Settings → AI Providers (instance-wide).'
    )
  }
  if (fixes.length === 0) {
    console.log(
      colors.success(
        'Ready — chat tools follow your user permissions and selected approval mode.'
      )
    )
  } else {
    console.log(colors.warning('Not fully ready:'))
    for (const fix of fixes) {
      console.log(`  ${icons.bullet} ${fix}`)
    }
  }
  newline()
}

/** Adapt the generated SDK to the small interface the sign-in flow uses. */
function providerLoginApi(): ProviderLoginApi {
  const unwrap = (result: {
    data?: LoginSession
    error?: unknown
  }): LoginSession => {
    if (result.error) throw new Error(getErrorMessage(result.error))
    if (!result.data) throw new Error('The server returned no sign-in state')
    return result.data
  }
  return {
    start: async (providerId) =>
      unwrap(
        await startAiProviderLogin({
          client,
          path: { provider_id: providerId },
        })
      ),
    status: async (providerId) =>
      unwrap(
        await getAiProviderLogin({ client, path: { provider_id: providerId } })
      ),
    submitCode: async (providerId, code) =>
      unwrap(
        await submitAiProviderLoginCode({
          client,
          path: { provider_id: providerId },
          body: { code },
        })
      ),
    cancel: async (providerId) =>
      unwrap(
        await cancelAiProviderLogin({
          client,
          path: { provider_id: providerId },
        })
      ),
  }
}

interface ProviderLoginOptions {
  json?: boolean
}

async function providerLogin(
  providerId: string,
  options: ProviderLoginOptions
): Promise<void> {
  await requireAuth()
  await setupClient()

  const api = providerLoginApi()
  let cancelSignIn: (() => Promise<void>) | undefined
  let interrupted = false
  const onInterrupt = () => {
    if (interrupted) process.exit(130)
    interrupted = true
    console.log(colors.warning('\nCancelling the sign-in…'))
    if (cancelSignIn) {
      void cancelSignIn().finally(() => process.exit(130))
    }
    // Otherwise the start request is still in flight: the flow cancels the
    // sign-in the moment it exists (see `shouldAbort`), then exits below.
  }
  process.on('SIGINT', onInterrupt)

  try {
    const session = await runProviderLogin(
      api,
      {
        log: (line) => console.log(line),
        promptCode: (message) => promptPassword({ message }),
        sleep: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
      },
      providerId,
      {
        onCancelable: (cancel) => {
          cancelSignIn = cancel
        },
        shouldAbort: () => interrupted,
      }
    )
    if (options.json) jsonOutput(session)
  } catch (error) {
    if (error instanceof LoginFlowError) {
      console.error(colors.error(`${icons.error} ${error.message}`))
      process.exitCode = interrupted ? 130 : 1
      return
    }
    throw error
  } finally {
    process.off('SIGINT', onInterrupt)
  }
}

export function registerAiCommands(program: Command): void {
  const ai = program.command('ai').description('AI assistant status for a project')

  const provider = ai
    .command('provider')
    .description('Manage the AI CLIs (Claude Code, Codex) Temps runs on this server')

  provider
    .command('login <provider-id>')
    .description(
      'Sign an AI CLI in to your subscription. Prints a link to open, then asks for the code (Claude) or shows a device code (Codex)'
    )
    .option('--json', 'Print the final sign-in state as JSON')
    .action((providerId, opts) => providerLogin(providerId, opts))

  ai.command('readiness [project]')
    .description('Show which AI prerequisites this project meets, and how to fix the rest')
    .option('-p, --project <project>', 'Project slug')
    .option('--json', 'Output in JSON format')
    .action((projectArg, opts) => {
      if (projectArg && !opts.project) {
        opts.project = projectArg
      }
      return readiness(projectArg, opts)
    })
}
