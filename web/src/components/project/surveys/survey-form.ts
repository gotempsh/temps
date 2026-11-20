// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  QuestionKind,
  SurveyQuestion,
  SurveyResponse,
  SurveyStatus,
  SurveyTargeting,
} from '@/api/client'

/** Editable survey state. Numbers stay strings until saved so a half-typed
 * value is not coerced while the user types. */
export interface SurveyDraft {
  name: string
  description: string
  questions: QuestionDraft[]
  pathPattern: string
  triggerEvent: string
  samplePercent: string
  oncePerVisitor: boolean
  flagKey: string
  flagValue: string
}

export interface QuestionDraft {
  id: string
  kind: QuestionKind
  prompt: string
  optional: boolean
  choices: string[]
  scale: number
  lowerLabel: string
  upperLabel: string
}

export const QUESTION_KINDS: { value: QuestionKind; label: string }[] = [
  { value: 'nps', label: 'NPS (0–10)' },
  { value: 'rating', label: 'Rating' },
  { value: 'single_choice', label: 'Single choice' },
  { value: 'multiple_choice', label: 'Multiple choice' },
  { value: 'open_text', label: 'Open text' },
]

export const RATING_SCALES = [3, 5, 7, 10]

export const STATUS_LABELS: Record<SurveyStatus, string> = {
  draft: 'Draft',
  active: 'Active',
  paused: 'Paused',
  completed: 'Completed',
}

/** Badge style per status: live is filled, a draft is outlined. */
export function statusVariant(
  status: SurveyStatus
): 'default' | 'secondary' | 'outline' {
  if (status === 'active') return 'default'
  if (status === 'draft') return 'outline'
  return 'secondary'
}

export function hasChoices(kind: QuestionKind): boolean {
  return kind === 'single_choice' || kind === 'multiple_choice'
}

export function isScale(kind: QuestionKind): boolean {
  return kind === 'rating' || kind === 'nps'
}

/**
 * A fresh question id, random rather than numbered. Answers are stored under
 * the id, and a survey does not remember the ids of questions removed in an
 * earlier edit, so any id derived from the questions still present could be
 * one a removed question used, and the new question would inherit its
 * answers. 10 base-36 characters make a repeat practically impossible.
 */
export function nextQuestionId(
  questions: QuestionDraft[],
  random: () => number = secureRandom
): string {
  const taken = new Set(questions.map((q) => q.id))
  for (;;) {
    let suffix = ''
    while (suffix.length < 10) {
      suffix += Math.floor(random() * 36).toString(36)
    }
    const id = `q_${suffix}`
    if (!taken.has(id)) return id
  }
}

/** A uniform number in [0, 1) from the platform CSPRNG. */
function secureRandom(): number {
  const word = new Uint32Array(1)
  crypto.getRandomValues(word)
  return word[0] / 2 ** 32
}

export function blankQuestion(
  questions: QuestionDraft[],
  kind: QuestionKind = 'nps'
): QuestionDraft {
  return {
    id: nextQuestionId(questions),
    kind,
    prompt:
      kind === 'nps' ? 'How likely are you to recommend us to a friend?' : '',
    optional: false,
    choices: hasChoices(kind) ? ['', ''] : [],
    scale: 5,
    lowerLabel: '',
    upperLabel: '',
  }
}

export function emptyDraft(): SurveyDraft {
  return {
    name: '',
    description: '',
    questions: [blankQuestion([])],
    pathPattern: '',
    triggerEvent: '',
    samplePercent: '100',
    oncePerVisitor: true,
    flagKey: '',
    flagValue: 'true',
  }
}

export function draftFromSurvey(survey: SurveyResponse): SurveyDraft {
  const targeting = survey.targeting
  return {
    name: survey.name,
    description: survey.description ?? '',
    questions: survey.questions.map((q) => ({
      id: q.id,
      kind: q.kind,
      prompt: q.prompt,
      optional: q.optional ?? false,
      choices: q.choices ?? [],
      scale: q.scale ?? 5,
      lowerLabel: q.lower_label ?? '',
      upperLabel: q.upper_label ?? '',
    })),
    pathPattern: targeting.path_pattern ?? '',
    triggerEvent: targeting.trigger_event ?? '',
    samplePercent: String(Math.round((targeting.sample_rate ?? 1) * 1000) / 10),
    oncePerVisitor: targeting.once_per_visitor ?? true,
    flagKey: targeting.flag?.key ?? '',
    flagValue:
      targeting.flag?.value === undefined
        ? 'true'
        : JSON.stringify(targeting.flag.value),
  }
}

export interface DraftErrors {
  name?: string
  questions?: string
  question: Record<number, string>
  pathPattern?: string
  triggerEvent?: string
  samplePercent?: string
  flagValue?: string
}

/** Check a draft against the same rules the server enforces, so the problem
 * shows next to its field. The server re-checks everything. */
export function validateDraft(
  draft: SurveyDraft,
  activating: boolean
): DraftErrors {
  const errors: DraftErrors = { question: {} }
  if (!draft.name.trim()) errors.name = 'Name the survey.'
  else if (draft.name.length > 200) errors.name = 'At most 200 characters.'

  if (draft.questions.length === 0 && activating) {
    errors.questions = 'An active survey needs at least one question.'
  } else if (draft.questions.length > 20) {
    errors.questions = 'At most 20 questions.'
  }
  const ids = new Set<string>()
  draft.questions.forEach((q, index) => {
    const problem = questionProblem(q, ids)
    if (problem) errors.question[index] = problem
    ids.add(q.id)
  })

  if (draft.pathPattern && !draft.pathPattern.startsWith('/')) {
    errors.pathPattern = "Start the path with '/', e.g. /checkout/*."
  }
  if (
    draft.triggerEvent &&
    !/^[A-Za-z0-9_.:-]{1,128}$/.test(draft.triggerEvent)
  ) {
    errors.triggerEvent =
      "Use letters, digits, '_', '-', '.' and ':' (the name you pass to trackEvent)."
  }
  const percent = Number(draft.samplePercent)
  if (
    draft.samplePercent.trim() === '' ||
    !Number.isFinite(percent) ||
    percent < 0 ||
    percent > 100
  ) {
    errors.samplePercent = 'Enter a percentage from 0 to 100.'
  }
  if (draft.flagKey.trim()) {
    try {
      JSON.parse(draft.flagValue)
    } catch {
      errors.flagValue = 'Enter a JSON value: true, "variant", 3 …'
    }
  }
  return errors
}

function questionProblem(
  q: QuestionDraft,
  earlierIds: Set<string>
): string | null {
  if (!/^[a-z0-9_]{1,32}$/.test(q.id)) {
    return 'The id must be 1–32 characters of a–z, 0–9 and _.'
  }
  if (earlierIds.has(q.id))
    return `Another question already uses the id "${q.id}".`
  if (!q.prompt.trim()) return 'Write the question.'
  if (q.prompt.length > 500) return 'At most 500 characters.'
  if (hasChoices(q.kind)) {
    const filled = q.choices.map((c) => c.trim())
    if (filled.length < 2 || filled.some((c) => !c))
      return 'Give at least two choices, none empty.'
    if (new Set(filled).size !== filled.length)
      return 'Each choice must be different.'
  }
  return null
}

export function hasErrors(errors: DraftErrors): boolean {
  return (
    Object.keys(errors.question).length > 0 ||
    Object.entries(errors).some(
      ([key, value]) => key !== 'question' && Boolean(value)
    )
  )
}

/** The request body fields for a draft. */
export function draftToBody(draft: SurveyDraft): {
  name: string
  description: string | null
  questions: SurveyQuestion[]
  targeting: SurveyTargeting
} {
  const flagKey = draft.flagKey.trim()
  return {
    name: draft.name.trim(),
    description: draft.description.trim() || null,
    questions: draft.questions.map((q) => ({
      id: q.id,
      kind: q.kind,
      prompt: q.prompt.trim(),
      optional: q.optional,
      ...(hasChoices(q.kind)
        ? { choices: q.choices.map((c) => c.trim()) }
        : {}),
      ...(q.kind === 'rating' ? { scale: q.scale } : {}),
      ...(isScale(q.kind) && q.lowerLabel.trim()
        ? { lower_label: q.lowerLabel.trim() }
        : {}),
      ...(isScale(q.kind) && q.upperLabel.trim()
        ? { upper_label: q.upperLabel.trim() }
        : {}),
    })),
    targeting: {
      path_pattern: draft.pathPattern.trim() || null,
      trigger_event: draft.triggerEvent.trim() || null,
      sample_rate: Number(draft.samplePercent) / 100,
      once_per_visitor: draft.oncePerVisitor,
      flag: flagKey
        ? { key: flagKey, value: JSON.parse(draft.flagValue) as unknown }
        : null,
    },
  }
}

/** One line describing when the survey shows, for the list and the header. */
export function describeTargeting(targeting: SurveyTargeting): string {
  const parts: string[] = []
  parts.push(
    targeting.path_pattern ? `On ${targeting.path_pattern}` : 'On every page'
  )
  if (targeting.trigger_event) parts.push(`after "${targeting.trigger_event}"`)
  const rate = targeting.sample_rate ?? 1
  if (rate < 1) parts.push(`${Math.round(rate * 1000) / 10}% of visitors`)
  if (targeting.flag)
    parts.push(
      `where ${targeting.flag.key} = ${JSON.stringify(targeting.flag.value)}`
    )
  if (targeting.once_per_visitor ?? true) parts.push('once per visitor')
  return parts.join(', ')
}

export function surveyErrorMessage(error: unknown, fallback: string): string {
  const problem = error as { detail?: string; title?: string } | null
  if (problem?.detail) return problem.detail
  if (problem?.title) return problem.title
  if (error instanceof Error && error.message) return error.message
  return fallback
}

/** The status changes offered from each status, in button order. */
export function statusActions(
  status: SurveyStatus
): { to: SurveyStatus; label: string }[] {
  switch (status) {
    case 'draft':
      return [{ to: 'active', label: 'Launch' }]
    case 'active':
      return [
        { to: 'paused', label: 'Pause' },
        { to: 'completed', label: 'Complete' },
      ]
    case 'paused':
      return [
        { to: 'active', label: 'Resume' },
        { to: 'completed', label: 'Complete' },
      ]
    case 'completed':
      return [{ to: 'active', label: 'Reopen' }]
  }
}

/** One stored answer as text: choices joined, numbers as written. */
export function formatAnswer(value: unknown): string {
  if (Array.isArray(value)) return value.map(String).join(', ')
  if (value === null || value === undefined) return ''
  return String(value)
}

/** Share of `part` in `whole` as a whole percentage, 0 when `whole` is 0. */
export function percentOf(part: number, whole: number): number {
  return whole > 0 ? Math.round((part / whole) * 100) : 0
}
