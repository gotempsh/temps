// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Answer state for one AskUserQuestion item: the options the user picked and,
 * when they chose "Other", what they wrote instead.
 */
export interface QuestionDraft {
  selected: string[]
  otherChosen: boolean
  otherText: string
}

export const EMPTY_DRAFT: QuestionDraft = {
  selected: [],
  otherChosen: false,
  otherText: '',
}

/**
 * Pick an option. A single-select question has one answer, so choosing an
 * option clears "Other"; a multi-select question toggles the option.
 */
export function chooseOption(
  draft: QuestionDraft,
  label: string,
  multiSelect: boolean
): QuestionDraft {
  if (!multiSelect) {
    return { ...draft, selected: [label], otherChosen: false }
  }
  const selected = draft.selected.includes(label)
    ? draft.selected.filter((l) => l !== label)
    : [...draft.selected, label]
  return { ...draft, selected }
}

/** Choose (or, for multi-select, toggle) the write-in "Other" answer. */
export function chooseOther(
  draft: QuestionDraft,
  multiSelect: boolean
): QuestionDraft {
  if (!multiSelect) {
    return { ...draft, selected: [], otherChosen: true }
  }
  return { ...draft, otherChosen: !draft.otherChosen }
}

/**
 * The answer sent back to the agent: the picked labels followed by the
 * write-in text, comma separated. Empty when nothing usable was given.
 */
export function composeAnswer(draft: QuestionDraft): string {
  const parts = [...draft.selected]
  const other = draft.otherText.trim()
  if (draft.otherChosen && other) parts.push(other)
  return parts.join(', ')
}

/**
 * Whether the question has an answer to send. Choosing "Other" without
 * writing anything does not count, so Send stays disabled until it does.
 */
export function isAnswered(draft: QuestionDraft): boolean {
  if (draft.otherChosen && !draft.otherText.trim()) return false
  return composeAnswer(draft).length > 0
}
