// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  WorkflowRevealTarget,
  WorkflowRunResponse,
  WorkflowStepResponse,
} from '@/api/client'
import { listWorkflowSteps } from '@/api/client/sdk.gen'

type RunGeneration = Pick<
  WorkflowRunResponse,
  'id' | 'attempt' | 'finished_at' | 'updated_at'
>

/**
 * Which failure of a run this is: a retried run that fails again has a new
 * attempt and finish time, so reads keyed on it never reuse the last one.
 */
export function failureGeneration(run: RunGeneration): string {
  return `${run.attempt}:${run.finished_at ?? run.updated_at}`
}

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

/** Every payload query of one run, for clearing them all at once. */
export function runPayloadsKey(runId: string) {
  return ['workflow-run-payload', runId] as const
}

/**
 * One encrypted payload of a failed run, keyed so a page reads it once per
 * failure. A retried run that fails again has a new attempt and finish time,
 * so it reads its new error instead of showing the cached one.
 */
export function payloadQueryKey(
  run: RunGeneration,
  target: WorkflowRevealTarget,
  sequence?: number
) {
  return [
    ...runPayloadsKey(run.id),
    failureGeneration(run),
    target,
    sequence ?? null,
  ] as const
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

/** Journal page size used to find a failed run's failed step. */
export const FAILED_STEP_PAGE_SIZE = 100

/**
 * The journal pages to read to find the step that failed a run: its last two
 * pages of {@link FAILED_STEP_PAGE_SIZE}. A failure ends the run, so the
 * failed step is among its last recorded steps, whichever page is on screen.
 */
export function failedStepPages(stepCount: number): number[] {
  if (stepCount <= 0) return []
  const last = Math.ceil(stepCount / FAILED_STEP_PAGE_SIZE)
  return last > 1 ? [last - 1, last] : [last]
}

/** The most recently recorded failed step across `pages`, oldest page first. */
export function findFailedStep<T extends Pick<WorkflowStepResponse, 'status'>>(
  pages: readonly (readonly T[])[]
): T | undefined {
  for (let page = pages.length - 1; page >= 0; page -= 1) {
    const steps = pages[page]
    for (let index = steps.length - 1; index >= 0; index -= 1) {
      if (steps[index].status === 'failed') return steps[index]
    }
  }
  return undefined
}

/**
 * One journal page read to find the step that failed `run`, keyed on the
 * failure's generation: a retry that fails at another step while the page
 * is open reads the journal again instead of naming the previous failure.
 */
export function failedStepQuery(
  path: { project_id: number; environment_id: number; run_id: string },
  run: RunGeneration,
  page: number
) {
  return {
    queryKey: [
      'workflow-run-failed-step',
      path,
      failureGeneration(run),
      page,
    ] as const,
    queryFn: async ({ signal }: { signal: AbortSignal }) =>
      (
        await listWorkflowSteps({
          path,
          query: { page, page_size: FAILED_STEP_PAGE_SIZE },
          signal,
          throwOnError: true,
        })
      ).data,
  }
}
