import { describe, expect, test } from 'bun:test'
import {
  AI_CHAT_PROMPT_CASES,
  renderAiChatPromptCasesMarkdown,
  validateAiChatPromptCases,
} from './ai-chat-prompts.ts'

describe('AI chat E2E prompt catalog', () => {
  test('all scenarios have unique, complete, internally consistent metadata', () => {
    expect(validateAiChatPromptCases(AI_CHAT_PROMPT_CASES)).toEqual([])
  })

  test('covers the requested infrastructure workflows and shared runtime contracts', () => {
    const ids = new Set(AI_CHAT_PROMPT_CASES.map((testCase) => testCase.id))
    for (const id of [
      'create-postgres',
      'create-mariadb',
      'create-s3',
      'link-postgres-and-redeploy',
      'create-load-balancer-route',
      'provision-managed-domain',
      'read-only-permission-boundary',
      'interrupt-long-turn',
    ]) {
      expect(ids.has(id)).toBe(true)
    }
  })

  test('markdown output includes runnable prompts, assertions, and cleanup', () => {
    const markdown = renderAiChatPromptCasesMarkdown(AI_CHAT_PROMPT_CASES)
    expect(markdown).toContain('## create-postgres')
    expect(markdown).toContain('> Create a standalone PostgreSQL service')
    expect(markdown).toContain('Assertions:')
    expect(markdown).toContain('Cleanup:')
  })
})
