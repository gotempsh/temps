// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** How an autofix incident's status reads in the console (ADR-054). */
export type IncidentTone = 'running' | 'review' | 'success' | 'muted' | 'error'

export interface IncidentStatusView {
  label: string
  tone: IncidentTone
}

const STATUS: Record<string, IncidentStatusView> = {
  open: { label: 'Queued', tone: 'running' },
  working: { label: 'Working', tone: 'running' },
  awaiting_review: { label: 'Waiting for review', tone: 'review' },
  merged: { label: 'Merged', tone: 'success' },
  applied: { label: 'Applied', tone: 'success' },
  rejected: { label: 'Rejected', tone: 'muted' },
  stopped: { label: 'Stopped', tone: 'muted' },
  failed: { label: 'Failed', tone: 'error' },
}

export function incidentStatus(status: string): IncidentStatusView {
  return STATUS[status] ?? { label: status, tone: 'muted' }
}

const PHASE: Record<string, string> = {
  queued: 'Waiting for the incident ahead to finish',
  prepare: 'Building the sandbox from the crashing deployment',
  reproduce: 'The agent is reproducing and fixing the crash',
  verify: 'Re-running the reproduction and tests',
  deliver: 'Delivering the fix',
  review: 'Waiting for you',
  done: 'Closed',
}

export function incidentPhase(phase: string): string {
  return PHASE[phase] ?? phase
}

/** True while an incident is queued or running, so the page keeps polling. */
export function isActive(status: string | undefined): boolean {
  return status === 'open' || status === 'working'
}

export interface DecisionOption {
  decision: 'merged' | 'applied' | 'rejected'
  label: string
}

/**
 * The decisions a reviewer can record for a fix, by what the fix became: a
 * pull request is merged, a patch is applied, an analysis is only accepted or
 * rejected (accepting it is recording that you applied it yourself).
 */
export function decisionsFor(
  delivery: string | null | undefined
): DecisionOption[] {
  const reject: DecisionOption = { decision: 'rejected', label: 'Reject' }
  switch (delivery) {
    case 'pull_request':
      return [{ decision: 'merged', label: 'I merged it' }, reject]
    case 'patch':
      return [{ decision: 'applied', label: 'I applied it' }, reject]
    default:
      return [{ decision: 'applied', label: 'I fixed it' }, reject]
  }
}

/** What a fix of this project becomes, said before the user starts one. */
export function deliveryLabel(delivery: string): string {
  switch (delivery) {
    case 'pull_request':
      return 'The fix is opened as a pull request on your repository.'
    case 'patch':
      return 'The fix is a patch you download and apply to your source.'
    default:
      return 'There is no source to change, so you get a written root-cause analysis.'
  }
}

/** Highest confidence level; the server scores fixes 0-5. */
export const MAX_CONFIDENCE = 5

/**
 * What each confidence level proves (ADR-054). A fix reaches a level only
 * when it meets that level's checks and every level below it. Mirrors
 * `LEVELS` in the server's `autofix/evidence.rs`.
 */
export const CONFIDENCE_LEVELS: { level: number; means: string }[] = [
  { level: 1, means: "The fix passes the agent's reproduction" },
  {
    level: 2,
    means:
      'The crash reproduces at the deployed source and the fix makes it pass',
  },
  {
    level: 3,
    means: 'The fix is small and changes a file from the stack trace',
  },
  { level: 4, means: "The project's test command passes" },
  { level: 5, means: 'Replaying the crashing request is clean' },
]

/** What a score proves, or that nothing was verified at 0. */
export function confidenceMeaning(score: number): string {
  return (
    CONFIDENCE_LEVELS.find((entry) => entry.level === score)?.means ??
    'Nothing was verified'
  )
}

/** Colour band of a confidence level out of 5. */
export function confidenceTone(score: number): 'high' | 'medium' | 'low' {
  if (score >= 4) return 'high'
  if (score >= 2) return 'medium'
  return 'low'
}

/**
 * The level a check counts toward, as `L1`-`L5`. A check that never scores
 * (the agent's own confidence) is shown for context only and reads as a dash.
 */
export function signalLevel(signal: { level?: number | null }): string {
  return signal.level == null ? '–' : `L${signal.level}`
}
