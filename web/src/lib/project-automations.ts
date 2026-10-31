// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Automation, as the project settings present it: a prompt plus a trigger,
 * where every run is an AI chat in the project that you can open and continue.
 *
 * Today two things fit that shape. Crash autofix already works in a chat, and
 * the repository-defined workflows (`.temps/agents`) run a prompt on a
 * trigger. This module turns both into the same plain-language rows, and
 * holds the starting prompts the page offers.
 */
import type { ConversationResponse } from '@/api/client/types.gen'

/** The trigger shape the workflow config stores (all keys optional). */
interface WorkflowTriggerConfig {
  error?: { new_issue?: boolean; regression?: boolean }
  deploy?: { production?: boolean; preview?: boolean }
  monitoring?: { downtime?: boolean; latency_spike?: boolean }
  schedule?: { cron?: string | null }
  manual?: boolean
  webhook?: boolean
}

const WEEKDAYS = [
  'Sunday',
  'Monday',
  'Tuesday',
  'Wednesday',
  'Thursday',
  'Friday',
  'Saturday',
]

/**
 * A five-field cron as words for the common shapes ("Every day at 09:00
 * UTC", "Every Monday at 08:30 UTC"); anything else is shown as written.
 */
export function describeCron(cron: string): string {
  const parts = cron.trim().split(/\s+/)
  if (parts.length !== 5) return `On schedule ${cron}`
  const [min, hour, dom, month, dow] = parts
  const numeric = /^\d+$/
  if (!numeric.test(min) || !numeric.test(hour) || dom !== '*' || month !== '*')
    return `On schedule ${cron}`
  const time = `${hour.padStart(2, '0')}:${min.padStart(2, '0')} UTC`
  if (dow === '*') return `Every day at ${time}`
  const day = numeric.test(dow) ? WEEKDAYS[Number(dow) % 7] : undefined
  return day ? `Every ${day} at ${time}` : `On schedule ${cron}`
}

/**
 * What starts a workflow, one phrase per trigger, in the order a person would
 * read them. An empty list means nothing starts it.
 */
export function describeWorkflowTriggers(config: unknown): string[] {
  if (!config || typeof config !== 'object') return []
  const t = config as WorkflowTriggerConfig
  const out: string[] = []
  if (t.error?.new_issue) out.push('A new error')
  if (t.error?.regression) out.push('A regression')
  if (t.deploy?.production) out.push('A production deploy')
  if (t.deploy?.preview) out.push('A preview deploy')
  if (t.monitoring?.downtime) out.push('Downtime')
  if (t.monitoring?.latency_spike) out.push('A latency spike')
  if (t.schedule?.cron) out.push(describeCron(t.schedule.cron))
  if (t.webhook) out.push('A webhook call')
  if (t.manual) out.push('On demand')
  return out
}

/** When crash autofix starts a chat, for each policy mode. */
export function autofixTriggerLabel(mode: string | undefined): string {
  switch (mode) {
    case 'propose':
      return 'Every new or regressed production crash'
    case 'auto_merge':
      return 'Every new or regressed production crash, merging confident fixes'
    default:
      return 'Only when someone presses "Fix this crash"'
  }
}

/** The project's live AI chats, most recently active first. */
export function recentProjectChats(
  conversations: readonly ConversationResponse[],
  limit = 8
): ConversationResponse[] {
  return conversations
    .filter((c) => c.kind === 'ai' && c.status !== 'archived')
    .sort(
      (a, b) =>
        new Date(b.last_activity_at).getTime() -
        new Date(a.last_activity_at).getTime()
    )
    .slice(0, limit)
}

/** Console URL of one chat. */
export function chatHref(publicId: string): string {
  return `/chat?chat=${encodeURIComponent(publicId)}`
}

export interface AutomationTemplate {
  id: string
  title: string
  /** The trigger it is meant for, in words. */
  when: string
  prompt: (projectName: string) => string
}

/** Starting points: each one is a prompt you can try in a chat right away. */
export const AUTOMATION_TEMPLATES: readonly AutomationTemplate[] = [
  {
    id: 'triage-new-errors',
    title: 'Triage new errors',
    when: 'A new error',
    prompt: (name) =>
      `A new error just appeared in ${name}. Find the most recent new error group, explain the likely cause from its stack trace and the deployment that introduced it, and say whether it needs a fix now.`,
  },
  {
    id: 'review-production-deploy',
    title: 'Check each production deploy',
    when: 'A production deploy',
    prompt: (name) =>
      `${name} was just deployed to production. Compare error rate, latency and request volume for the 30 minutes after the latest production deployment with the 30 minutes before it, and tell me whether anything regressed.`,
  },
  {
    id: 'investigate-downtime',
    title: 'Investigate downtime',
    when: 'Downtime',
    prompt: (name) =>
      `An uptime monitor for ${name} reported downtime. Look at the monitor results, the latest deployments, container status and application logs around that time, and tell me what went down and why.`,
  },
  {
    id: 'weekly-summary',
    title: 'Weekly health summary',
    when: 'Every Monday at 09:00 UTC',
    prompt: (name) =>
      `Summarise the last 7 days of ${name}: deployments, new and recurring errors, slowest endpoints, uptime, and anything that got worse compared with the week before.`,
  },
]
