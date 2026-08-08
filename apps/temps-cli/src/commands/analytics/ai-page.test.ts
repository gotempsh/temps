import { test, expect, describe } from 'bun:test'
import { buildAiAgentRows, type AiAgentBreakdownItem } from './ai-page.js'

const items: AiAgentBreakdownItem[] = [
  { provider: 'OpenAI', agent: 'GPTBot', request_count: 60, unique_ips: 10 },
  { provider: 'OpenAI', agent: 'ChatGPT-User', request_count: 20, unique_ips: 5 },
  { provider: 'Anthropic', agent: 'ClaudeBot', request_count: 20, unique_ips: 4 },
]

describe('buildAiAgentRows (ai-page drill-down)', () => {
  test('group by agent: sorted descending, one row per agent', () => {
    const rows = buildAiAgentRows(items, 'agent')
    expect(rows.map((r) => r.agent)).toEqual(['GPTBot', 'ChatGPT-User', 'ClaudeBot'])
  })

  test('group by provider: aggregates counts and sorts by total', () => {
    const rows = buildAiAgentRows(items, 'provider')
    expect(rows.map((r) => r.provider)).toEqual(['OpenAI', 'Anthropic'])
    expect(rows[0]!.requestCount).toBe(80)
    expect(rows[0]!.uniqueIps).toBe(15)
  })

  test('an empty breakdown for this path produces no rows', () => {
    expect(buildAiAgentRows([], 'agent')).toEqual([])
  })
})
