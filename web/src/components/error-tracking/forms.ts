// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'

/** Longest comment the server accepts. */
export const MAX_COMMENT_CHARS = 10_000
/** Longest saved-search name the server accepts. */
export const MAX_SAVED_SEARCH_NAME_CHARS = 100

/** A comment on an error group, new or edited. */
export const commentFormSchema = z.object({
  body: z
    .string()
    .trim()
    .min(1, 'Write something before sending the comment.')
    .max(
      MAX_COMMENT_CHARS,
      `A comment can be at most ${MAX_COMMENT_CHARS.toLocaleString('en-US')} characters.`
    ),
})
export type CommentFormInput = z.input<typeof commentFormSchema>
export type CommentFormData = z.output<typeof commentFormSchema>

/** The name and visibility of a saved search over the error list. */
export const savedSearchFormSchema = z.object({
  name: z
    .string()
    .trim()
    .min(1, 'Give the search a name.')
    .max(
      MAX_SAVED_SEARCH_NAME_CHARS,
      `A name can be at most ${MAX_SAVED_SEARCH_NAME_CHARS} characters.`
    ),
  shared: z.boolean(),
})
export type SavedSearchFormInput = z.input<typeof savedSearchFormSchema>
export type SavedSearchFormData = z.output<typeof savedSearchFormSchema>
