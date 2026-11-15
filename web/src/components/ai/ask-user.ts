// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ChatPart } from './chat-message-parts'

/** Tool the assistant calls to ask the user a question mid-turn. */
export const ASK_USER_TOOL_NAME = 'temps_ask_user'

export function isAskUserTool(name: string): boolean {
  return name === ASK_USER_TOOL_NAME || name.endsWith(`__${ASK_USER_TOOL_NAME}`)
}

export interface AskUserAnswer {
  question: string
  header: string | null
  /** Empty when the user left the question blank. */
  answer: string
}

/** The saved outcome of a question, read from the tool's result. */
export type AskUserOutcome =
  | { status: 'answered'; answers: AskUserAnswer[] }
  | { status: 'not_answered'; reason: string | null }

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value)

const text = (value: unknown): string | null =>
  typeof value === 'string' && value.trim().length > 0 ? value.trim() : null

/**
 * `null` while the question is open (no result yet) and when the result is a
 * validation message rather than an outcome, which the generic tool card shows.
 */
export function parseAskUserResult(
  result: string | null | undefined
): AskUserOutcome | null {
  if (!result) return null
  let value: unknown
  try {
    value = JSON.parse(result)
  } catch {
    return null
  }
  if (!isRecord(value)) return null
  if (value.status === 'not_answered') {
    return { status: 'not_answered', reason: text(value.reason) }
  }
  if (value.status !== 'answered' || !Array.isArray(value.answers)) return null
  const answers = value.answers.flatMap((item): AskUserAnswer[] => {
    if (!isRecord(item)) return []
    const question = text(item.question)
    if (!question) return []
    return [
      {
        question,
        header: text(item.header),
        answer: typeof item.answer === 'string' ? item.answer.trim() : '',
      },
    ]
  })
  return answers.length > 0 ? { status: 'answered', answers } : null
}

function questionsKey(questions: string[]): string {
  return questions.join('\u0000')
}

function permissionQuestions(input: unknown): string[] {
  if (!isRecord(input) || !Array.isArray(input.questions)) return []
  return input.questions.flatMap((item) => {
    const question = isRecord(item) ? text(item.question) : null
    return question ? [question] : []
  })
}

/**
 * Question cards whose answers are already saved on their tool call. The live
 * card turns into "You answered…" when it is resolved, and the saved answers
 * appear on the tool call a moment later; showing both would repeat the
 * exchange. Matching is by the questions asked, so a second, still-open
 * question in the same turn stays visible.
 */
export function answeredQuestionPermissionIds(parts: ChatPart[]): Set<string> {
  const answered = new Set<string>()
  for (const part of parts) {
    if (part.type !== 'tool' || !isAskUserTool(part.tool.name)) continue
    const outcome = parseAskUserResult(part.tool.result)
    if (outcome?.status === 'answered') {
      answered.add(questionsKey(outcome.answers.map((a) => a.question)))
    }
  }
  const ids = new Set<string>()
  if (answered.size === 0) return ids
  for (const part of parts) {
    if (
      part.type === 'permission' &&
      part.permission.kind === 'question' &&
      isAskUserTool(part.permission.tool_name) &&
      answered.has(questionsKey(permissionQuestions(part.permission.input)))
    ) {
      ids.add(part.permission.id)
    }
  }
  return ids
}
