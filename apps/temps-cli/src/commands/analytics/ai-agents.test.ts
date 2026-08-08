import { test, expect, describe } from 'bun:test'
import { buildAiAgentRows, type AiAgentBreakdownItem } from './ai-agents.js'

const items: AiAgentBreakdownItem[] = [
  { provider: 'OpenAI', agent: 'GPTBot', purpose: 'training', request_count: 60, unique_ips: 10 },
  { provider: 'OpenAI', agent: 'ChatGPT-User', purpose: 'search', request_count: 20, unique_ips: 5 },
  { provider: 'Anthropic', agent: 'ClaudeBot', purpose: 'training', request_count: 20, unique_ips: 4 },
]

describe('buildAiAgentRows (group by agent)', () => {
  test('one row per agent, sorted by request count descending', () => {
    const rows = buildAiAgentRows(items, 'agent')
    expect(rows.map((r) => r.agent)).toEqual(['GPTBot', 'ChatGPT-User', 'ClaudeBot'])
  })

  test('percentage is share of total requests across all agents', () => {
    const rows = buildAiAgentRows(items, 'agent')
    const gptBot = rows.find((r) => r.agent === 'GPTBot')!
    expect(gptBot.percentage).toBeCloseTo(60)
  })

  test('missing purpose defaults to an empty string, not "undefined"', () => {
    const rows = buildAiAgentRows(
      [{ provider: 'OpenAI', agent: 'GPTBot', request_count: 10, unique_ips: 1 }],
      'agent'
    )
    expect(rows[0]!.purpose).toBe('')
  })

  test('an empty breakdown produces no rows and no divide-by-zero NaN', () => {
    expect(buildAiAgentRows([], 'agent')).toEqual([])
  })
})

describe('buildAiAgentRows (group by provider)', () => {
  test('aggregates request and IP counts per provider', () => {
    const rows = buildAiAgentRows(items, 'provider')
    const openai = rows.find((r) => r.provider === 'OpenAI')!
    expect(openai.requestCount).toBe(80)
    expect(openai.uniqueIps).toBe(15)
  })

  test('sorts providers by aggregated request count descending', () => {
    const rows = buildAiAgentRows(items, 'provider')
    expect(rows.map((r) => r.provider)).toEqual(['OpenAI', 'Anthropic'])
  })

  test('percentage reflects the provider share of total requests', () => {
    const rows = buildAiAgentRows(items, 'provider')
    const anthropic = rows.find((r) => r.provider === 'Anthropic')!
    expect(anthropic.percentage).toBeCloseTo(20)
  })
})
