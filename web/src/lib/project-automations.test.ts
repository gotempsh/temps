// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import type { ConversationResponse } from '@/api/client/types.gen'
import {
  AUTOMATION_TEMPLATES,
  autofixTriggerLabel,
  chatHref,
  describeCron,
  describeWorkflowTriggers,
  recentProjectChats,
} from './project-automations'

function chat(
  publicId: string,
  lastActivity: string,
  overrides: Partial<ConversationResponse> = {}
): ConversationResponse {
  return {
    public_id: publicId,
    last_activity_at: lastActivity,
    created_at: lastActivity,
    kind: 'ai',
    status: 'active',
    turn_status: 'idle',
    context_type: 'project',
    context_id: '1',
    ai_model: 'model',
    ai_provider: 'gateway',
    ai_permission_mode: 'default',
    ...overrides,
  }
}

describe('describeCron', () => {
  it('reads daily and weekly schedules', () => {
    expect(describeCron('0 9 * * *')).toBe('Every day at 09:00 UTC')
    expect(describeCron('30 8 * * 1')).toBe('Every Monday at 08:30 UTC')
    expect(describeCron('0 0 * * 7')).toBe('Every Sunday at 00:00 UTC')
  })

  it('shows anything else as written', () => {
    expect(describeCron('*/5 * * * *')).toBe('On schedule */5 * * * *')
    expect(describeCron('0 9 1 * *')).toBe('On schedule 0 9 1 * *')
    expect(describeCron('daily')).toBe('On schedule daily')
  })
})

describe('describeWorkflowTriggers', () => {
  it('lists every configured trigger in reading order', () => {
    expect(
      describeWorkflowTriggers({
        manual: true,
        schedule: { cron: '0 9 * * 1' },
        error: { new_issue: true, regression: true },
        deploy: { production: true },
        monitoring: { downtime: true },
      })
    ).toEqual([
      'A new error',
      'A regression',
      'A production deploy',
      'Downtime',
      'Every Monday at 09:00 UTC',
      'On demand',
    ])
  })

  it('returns nothing for an empty or malformed config', () => {
    expect(describeWorkflowTriggers({})).toEqual([])
    expect(describeWorkflowTriggers(null)).toEqual([])
    expect(describeWorkflowTriggers('error')).toEqual([])
  })
})

describe('autofixTriggerLabel', () => {
  it('names when each mode starts a chat', () => {
    expect(autofixTriggerLabel('propose')).toContain('Every new')
    expect(autofixTriggerLabel('auto_merge')).toContain('merging')
    expect(autofixTriggerLabel('off')).toContain('Fix this crash')
    expect(autofixTriggerLabel(undefined)).toContain('Fix this crash')
  })
})

describe('recentProjectChats', () => {
  it('keeps live AI chats, newest activity first, up to the limit', () => {
    const chats = [
      chat('old', '2026-10-01T00:00:00Z'),
      chat('terminal', '2026-10-29T00:00:00Z', { kind: 'terminal' }),
      chat('archived', '2026-10-28T00:00:00Z', { status: 'archived' }),
      chat('new', '2026-10-27T00:00:00Z'),
      chat('mid', '2026-10-10T00:00:00Z'),
    ]
    expect(recentProjectChats(chats).map((c) => c.public_id)).toEqual([
      'new',
      'mid',
      'old',
    ])
    expect(recentProjectChats(chats, 1).map((c) => c.public_id)).toEqual([
      'new',
    ])
  })

  it('does not reorder the list it was given', () => {
    const chats = [
      chat('a', '2026-10-01T00:00:00Z'),
      chat('b', '2026-10-02T00:00:00Z'),
    ]
    recentProjectChats(chats)
    expect(chats.map((c) => c.public_id)).toEqual(['a', 'b'])
  })
})

describe('templates', () => {
  it('have unique ids and name the project in their prompt', () => {
    const ids = AUTOMATION_TEMPLATES.map((t) => t.id)
    expect(new Set(ids).size).toBe(ids.length)
    for (const template of AUTOMATION_TEMPLATES) {
      expect(template.prompt('shop-api')).toContain('shop-api')
    }
  })

  it('link to a chat by its public id', () => {
    expect(chatHref('c_1 2')).toBe('/chat?chat=c_1%202')
  })
})
