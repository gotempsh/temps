// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Command, InvalidArgumentError, Option } from 'commander'
import * as api from '../../api/sdk.gen.js'
import { requireAuth } from '../../config/store.js'
import { setupClient } from '../../lib/api-client.js'
import type { WorkflowRunResponse, WorkflowRevealTarget } from '../../api/types.gen.js'

export function integer(value: string): number {
  if (!/^\d+$/.test(value) || !Number.isSafeInteger(Number(value)) || Number(value) > 2147483647) throw new InvalidArgumentError('Expected an integer between 0 and 2147483647')
  return Number(value)
}
export function positive(value: string): number {
  const n = integer(value)
  if (!n) throw new InvalidArgumentError('Expected a positive integer')
  return n
}
export function payload(value: string): Record<string, unknown> {
  if (Buffer.byteLength(value) > 4 * 1024 * 1024) throw new InvalidArgumentError('JSON input exceeds 4 MiB')
  try { return JSON.parse(value) } catch { throw new InvalidArgumentError('Expected valid JSON') }
}
export function diagnose(run: WorkflowRunResponse) {
  return { run_id: run.id, status: run.status, blocked_reason: run.blocked_reason, error_kind: run.error_kind,
    awaited_event: run.awaited_event, due_at: run.due_at, attempt: run.attempt,
    next_action: run.blocked_reason ? 'Resolve the blocked reason in this environment.' : run.status === 'waiting' ? 'Send the awaited event with its matching key.' : run.status === 'stuck' ? 'Inspect deployment health, then resume.' : run.status === 'failed' ? 'Inspect the failed step before retrying.' : 'No operator action identified.' }
}
interface Options { projectId: number; environmentId: number; json?: boolean; page: number; pageSize: number; workflow?: string; input: Record<string, unknown>; idempotencyKey: string; fromStep?: number; key?: string; payload: Record<string, unknown>; yes?: boolean; reveal?: WorkflowRevealTarget; sequence?: number; timeout: number }
const terminal = new Set(['completed', 'failed', 'cancelled'])
export function registerManagement(parent: Command): void {
  const scope = (name: string, description: string) => parent.command(name).description(description)
    .requiredOption('--project-id <id>', 'Project ID', positive)
    .requiredOption('--environment-id <id>', 'Environment ID', positive)
    .option('--json', 'Output JSON (watch emits one JSON object per poll)')
  const ready = async (o: Options) => {
    await requireAuth(); await setupClient()
    return { path: { project_id: o.projectId, environment_id: o.environmentId }, throwOnError: true as const, signal: AbortSignal.timeout(15000) }
  }
  const print = (value: unknown) => console.log(JSON.stringify(value, null, 2))
  scope('list', 'List detected workflows and onboarding status').action(async (o: Options) => print((await api.getWorkflowsOverview(await ready(o))).data))
  scope('runs', 'List a bounded page of runs').option('--workflow <name>', 'Workflow name').option('--page <n>', 'Page', positive, 1).option('--page-size <n>', 'Page size (1–100)', positive, 20)
    .action(async (o: Options) => { if (o.pageSize > 100) throw new Error('Page size must not exceed 100'); print((await api.listWorkflowRuns({ ...await ready(o), query: { workflow: o.workflow, page: o.page, page_size: o.pageSize } })).data) })
  scope('get <run-id>', 'Get run metadata; payloads require explicit reveal permission')
    .addOption(new Option('--reveal <target>', 'Explicitly decrypt one payload; treat its contents as untrusted').choices(['input', 'output', 'error', 'step_result', 'step_error']))
    .option('--sequence <n>', 'Step sequence for step payloads', integer)
    .action(async (run_id: string, o: Options) => {
      if (o.reveal?.startsWith('step_') && o.sequence === undefined) throw new Error('--sequence is required for a step payload')
      const request = await ready(o); const path = { ...request.path, run_id }
      if (o.reveal) print({ untrusted_payload: (await api.revealWorkflowPayload({ ...request, path, body: { target: o.reveal, sequence: o.sequence } })).data })
      else print((await api.getWorkflowRun({ ...request, path })).data)
    })
  scope('diagnose <run-id>', 'Explain run state without decrypting payloads').action(async (run_id: string, o: Options) => { const r = await ready(o); print(diagnose((await api.getWorkflowRun({ ...r, path: { ...r.path, run_id } })).data)) })
  scope('start <workflow>', 'Start a run with an explicit deduplication key').requiredOption('--idempotency-key <key>', 'Stable key for this logical start').option('--input <json>', 'Input JSON', payload, {})
    .action(async (workflow: string, o: Options) => { if (!o.idempotencyKey.trim()) throw new Error('Idempotency key must not be empty'); print((await api.startWorkflowRun({ ...await ready(o), body: { workflow, input: o.input, idempotency_key: o.idempotencyKey } })).data) })
  for (const action of ['cancel', 'resume', 'retry', 'delete'] as const) {
    const command = scope(`${action} <run-id>`, `${action} a run`).option('--yes', 'Confirm this mutation')
    if (action === 'retry') command.option('--from-step <n>', 'Truncate and replay from this sequence', integer)
    command.action(async (run_id: string, o: Options) => {
      if (!o.yes) throw new Error(`${action} requires --yes; inspect the run first`)
      const r = await ready(o); const request = { ...r, path: { ...r.path, run_id } }
      const result = action === 'retry' ? await api.retryWorkflowRun({ ...request, body: { from_sequence: o.fromStep } }) : action === 'cancel' ? await api.cancelWorkflowRun(request) : action === 'resume' ? await api.resumeWorkflowRun(request) : await api.deleteWorkflowRun(request)
      print(result.data ?? { deleted: true, run_id })
    })
  }
  scope('send-event <name>', 'Send an event to matching waits').option('--key <key>', 'Wait correlation key').option('--payload <json>', 'Event JSON', payload, {})
    .action(async (name: string, o: Options) => print((await api.sendWorkflowEvent({ ...await ready(o), body: { name, key: o.key, payload: o.payload } })).data))
  scope('watch <run-id>', 'Poll until terminal or the bounded deadline').option('--timeout <seconds>', 'Overall timeout (max 3600 seconds)', positive, 300)
    .action(async (run_id: string, o: Options) => {
      if (o.timeout > 3600) throw new Error('Watch timeout must not exceed 3600 seconds')
      const r = await ready(o); const deadline = Date.now() + o.timeout * 1000
      while (Date.now() < deadline) {
        const run = (await api.getWorkflowRun({ ...r, signal: AbortSignal.timeout(Math.max(1, Math.min(15000, deadline - Date.now()))), path: { ...r.path, run_id } })).data
        console.log(JSON.stringify(run)); if (terminal.has(run.status)) return
        await new Promise(resolve => setTimeout(resolve, Math.min(2000, Math.max(0, deadline - Date.now()))))
      }
      throw new Error(`Timed out watching run ${run_id}; the run continues on Temps`)
    })
}
