// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  AI_CHAT_PROMPT_CASES,
  renderAiChatPromptCasesMarkdown,
  validateAiChatPromptCases,
  type AiChatPromptCategory,
} from '../lib/ai-chat-prompts.ts'

export interface AiChatPromptsOptions {
  category?: string
  destructiveOnly?: boolean
  json?: boolean
}

export function aiChatPromptsCommand(opts: AiChatPromptsOptions): void {
  const validationErrors = validateAiChatPromptCases(AI_CHAT_PROMPT_CASES)
  if (validationErrors.length > 0) {
    throw new Error(`Invalid AI chat prompt catalog:\n${validationErrors.join('\n')}`)
  }

  const category = opts.category as AiChatPromptCategory | undefined
  const cases = AI_CHAT_PROMPT_CASES.filter(
    (testCase) =>
      (!category || testCase.category === category) &&
      (!opts.destructiveOnly || testCase.destructive),
  )
  if (category && cases.length === 0) {
    throw new Error(`Unknown or empty AI chat prompt category: ${category}`)
  }

  if (opts.json) {
    process.stdout.write(`${JSON.stringify(cases, null, 2)}\n`)
    return
  }
  process.stdout.write(renderAiChatPromptCasesMarkdown(cases))
}
