// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { z } from 'zod'
import { pluginGrantsSchema } from './plugin-grants'

export const repositoryInstallSchema = z.object({
  name: z
    .string()
    .regex(
      /^[a-z0-9][a-z0-9-]{0,63}$/,
      'Use the plugin name declared by its package.'
    )
    .or(z.literal(''))
    .optional(),
  repository_url: z
    .string()
    .regex(
      /^https:\/\/github\.com\/[A-Za-z0-9][A-Za-z0-9_.-]*\/[A-Za-z0-9][A-Za-z0-9_.-]*$/,
      'Use https://github.com/owner/repository without credentials.'
    ),
  ref_name: z
    .string()
    .min(1)
    .max(128)
    .regex(/^[A-Za-z0-9][A-Za-z0-9_./-]*$/, 'Enter a branch, tag, or commit.')
    .refine(
      (value) => !value.includes('..'),
      'Git refs cannot contain two consecutive dots.'
    )
    .or(z.literal(''))
    .optional(),
  trusted: z
    .boolean()
    .refine(Boolean, 'Confirm you trust this plugin before installing.'),
  grants: pluginGrantsSchema.optional(),
})
export type RepositoryInstallValues = z.infer<typeof repositoryInstallSchema>

export type RepositorySelection = {
  name: string
  repository: string
  commit: string
}

/** Catalog selection pins the reviewed revision, but never grants execution consent. */
export function repositorySelectionValues(
  selection?: RepositorySelection | null
): RepositoryInstallValues {
  return {
    name: selection?.name ?? '',
    repository_url: selection?.repository ?? '',
    ref_name: selection?.commit ?? '',
    trusted: false,
  }
}

export function repositoryInstallBody(values: RepositoryInstallValues) {
  return {
    repository_url: values.repository_url,
    ...(values.name ? { name: values.name } : {}),
    ...(values.ref_name ? { ref_name: values.ref_name } : {}),
    ...(values.grants ? { grants: values.grants } : {}),
  }
}
