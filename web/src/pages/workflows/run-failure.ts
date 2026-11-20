// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  WorkflowRevealTarget,
  WorkflowRunResponse,
  WorkflowStepResponse,
} from '@/api/client'

type RunFailureFields = Pick<WorkflowRunResponse, 'error_kind' | 'step_count'>
type FailedStep = Pick<WorkflowStepResponse, 'sequence' | 'name' | 'attempts'>

/** What went wrong, in words, from the run's error kind and failed step. */
export function failureTitle(run: RunFailureFields, step?: FailedStep): string {
  switch (run.error_kind) {
    case 'step_failed':
      return step
        ? `Step ${step.sequence + 1}, ${step.name}, failed after ${step.attempts} ${step.attempts === 1 ? 'attempt' : 'attempts'}`
        : 'A step failed on every attempt'
    case 'workflow_failed':
      return run.step_count === 0
        ? 'The workflow failed before its first step'
        : 'The workflow threw an error outside its steps'
    case 'incompatible':
      return 'The deployed code no longer matches this run'
    case 'payload_too_large':
      return 'A payload was larger than the limit'
    case 'cancelled':
      return 'The run was cancelled'
    default:
      return 'The run failed'
  }
}

/** The one thing to check first for each kind of failure. */
export function failureHint(run: RunFailureFields): string | null {
  switch (run.error_kind) {
    case 'workflow_failed':
      return run.step_count === 0
        ? 'This usually means the input is not what the workflow expects.'
        : null
    case 'incompatible':
      return 'The code now makes different steps than the journal recorded. Retry from an earlier step, or deploy the code the run started with.'
    case 'payload_too_large':
      return 'Return less data from the step, or store it elsewhere and return a reference.'
    default:
      return null
  }
}

/** One encrypted payload of a failed run, keyed so a page shows it once. */
export function payloadQueryKey(
  runId: string,
  target: WorkflowRevealTarget,
  sequence?: number
) {
  return ['workflow-run-payload', runId, target, sequence ?? null] as const
}

/** Lines shown before a long payload is trimmed behind "Show all". */
export const PAYLOAD_PREVIEW_LINES = 8
/** Characters shown before a long single-line payload is trimmed. */
export const PAYLOAD_PREVIEW_CHARS = 600

/** The trimmed preview of `text`, or null when it is short enough to show whole. */
export function payloadPreview(text: string): string | null {
  const lines = text.split('\n')
  let preview = text
  if (lines.length > PAYLOAD_PREVIEW_LINES)
    preview = lines.slice(0, PAYLOAD_PREVIEW_LINES).join('\n')
  if (preview.length > PAYLOAD_PREVIEW_CHARS)
    preview = preview.slice(0, PAYLOAD_PREVIEW_CHARS)
  return preview === text ? null : `${preview.trimEnd()}…`
}

/** The HTTP status of a Problem Details error, when it has one. */
export function problemStatus(error: unknown): number | undefined {
  if (typeof error === 'object' && error !== null && 'status' in error) {
    const status = (error as { status: unknown }).status
    return typeof status === 'number' ? status : undefined
  }
  return undefined
}

export function payloadText(value: unknown): string {
  if (value === null || value === undefined) return 'Nothing was stored.'
  if (typeof value === 'string') return value.trim()
  return JSON.stringify(value, null, 2)
}
