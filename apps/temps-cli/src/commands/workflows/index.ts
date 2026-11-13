// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Command, InvalidArgumentError } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient } from '../../lib/api-client.js'
import { info } from '../../ui/output.js'
import { openWorkflowDev, configureWorkflowDev, pollWorkflowDev, respondWorkflowDev, closeWorkflowDev } from '../../api/sdk.gen.js'
import type { WorkflowManifest, WorkflowDevReply } from '../../api/types.gen.js'
import { runTunnel, type TunnelApi } from './tunnel.js'
function id(value: string): number {
  const parsed = Number(value)
  if (!/^[1-9]\d*$/.test(value) || !Number.isSafeInteger(parsed) || parsed > 2147483647) throw new InvalidArgumentError('Expected a positive project or environment ID')
  return parsed
}
export function registerDurableWorkflowCommands(program: Command): void {
  program.command('workflows').description('Durable application workflows')
    .command('dev').description('Forward preview-environment invocations to a local SDK server')
    .requiredOption('--project-id <id>', 'Project ID', id)
    .requiredOption('--environment-id <id>', 'Undeployed, unprotected preview environment ID', id)
    .option('--port <port>', 'Local SDK server port', '3000')
    .action(async (options: { projectId: number; environmentId: number; port: string }) => {
      await requireAuth()
      await setupClient()
      const path = { project_id: options.projectId, environment_id: options.environmentId }
      const timeout = () => AbortSignal.timeout(15000)
      const api: TunnelApi = {
        open: async () => (await openWorkflowDev({ path, throwOnError: true, signal: timeout() })).data,
        configure: async (session_id, manifest) => { await configureWorkflowDev({ path: { ...path, session_id }, body: manifest as WorkflowManifest, throwOnError: true, signal: timeout() }) },
        poll: async (session_id) => (await pollWorkflowDev({ path: { ...path, session_id }, throwOnError: true, signal: timeout() })).data,
        respond: async (session_id, request_id, reply) => { await respondWorkflowDev({ path: { ...path, session_id, request_id }, body: reply as WorkflowDevReply, throwOnError: true, signal: timeout() }) },
        close: async (session_id) => { await closeWorkflowDev({ path: { ...path, session_id }, throwOnError: true, signal: timeout() }) },
      }
      const stop = new AbortController()
      const abort = () => stop.abort()
      process.once('SIGINT', abort)
      process.once('SIGTERM', abort)
      try {
        await runTunnel(api, options.port, stop.signal, () => info(`Workflow development tunnel ready for project ${options.projectId}, environment ${options.environmentId}, local port ${options.port}. Start a run in the console. Ctrl-C disconnects.`))
      } finally {
        process.removeListener('SIGINT', abort)
        process.removeListener('SIGTERM', abort)
      }
    })
}
