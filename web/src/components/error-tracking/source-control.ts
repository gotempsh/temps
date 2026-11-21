// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'
import type {
  OwnershipRuleRequest,
  OwnershipRuleView,
} from '@/api/client/types.gen'

/** Most issues linked to one group (the server's limit). */
export const MAX_LINKS_PER_GROUP = 10
/** Longest linked URL. */
export const MAX_URL_CHARS = 2048
/** Longest issue title. */
export const MAX_ISSUE_TITLE_CHARS = 255
/** Most ownership rules of a project. */
export const MAX_OWNERSHIP_RULES = 50
/** Longest ownership pattern. */
export const MAX_PATTERN_CHARS = 300
/** Where a server without the git integration sends the user. */
export const GIT_PROVIDERS_PATH = '/git-providers'

/** Link an existing issue by its URL. */
export const linkIssueSchema = z.object({
  url: z
    .string()
    .trim()
    .min(1, 'Paste the issue URL')
    .max(MAX_URL_CHARS, `At most ${MAX_URL_CHARS} characters`)
    .refine((value) => {
      try {
        const url = new URL(value)
        return (
          (url.protocol === 'http:' || url.protocol === 'https:') &&
          !url.username &&
          !url.password
        )
      } catch {
        return false
      }
    }, 'An http or https URL, without a user name or password'),
  title: z
    .string()
    .trim()
    .max(MAX_ISSUE_TITLE_CHARS, `At most ${MAX_ISSUE_TITLE_CHARS} characters`),
})
export type LinkIssueFormData = z.infer<typeof linkIssueSchema>

/** Open an issue: an optional title, else the group's. */
export const openIssueSchema = z.object({
  title: z
    .string()
    .trim()
    .max(MAX_ISSUE_TITLE_CHARS, `At most ${MAX_ISSUE_TITLE_CHARS} characters`),
})
export type OpenIssueFormData = z.infer<typeof openIssueSchema>

/** Whether a pattern is a repository path, as the server checks it. */
export function patternProblem(pattern: string): string | null {
  const value = pattern.trim().replace(/^\.\//, '').replace(/^\/+/, '')
  if (!value) return 'Enter a path such as src/billing/'
  if (value.length > MAX_PATTERN_CHARS)
    return `At most ${MAX_PATTERN_CHARS} characters`
  if (value.includes('\\')) return "Use '/' between directories"
  if (value.split('/').includes('..')) return "A pattern cannot contain '..'"
  return null
}

const ruleSchema = z.object({
  pattern: z.string().superRefine((value, ctx) => {
    const problem = patternProblem(value)
    if (problem) ctx.addIssue({ code: 'custom', message: problem })
  }),
  owner_user_id: z
    .number({ message: 'Choose who owns it' })
    .int()
    .positive('Choose who owns it'),
  owner_label: z.string(),
})

/** The ownership rules editor: the rules in the order they are tried. */
export const ownershipRulesSchema = z.object({
  rules: z
    .array(ruleSchema)
    .max(MAX_OWNERSHIP_RULES, `At most ${MAX_OWNERSHIP_RULES} rules`),
})
export type OwnershipRulesFormData = z.infer<typeof ownershipRulesSchema>

/** The rules as the form holds them. */
export function rulesToForm(
  rules: OwnershipRuleView[]
): OwnershipRulesFormData {
  return {
    rules: rules.map((rule) => ({
      pattern: rule.pattern,
      owner_user_id: rule.owner_user_id,
      owner_label: rule.owner_name ?? `User #${rule.owner_user_id}`,
    })),
  }
}

/**
 * Whether a repository path matches a pattern, as the server matches it:
 * `src/billing/` is everything under the directory, `*.py` a file name in
 * any directory, `**` any number of directories, `*` and `?` within one.
 */
export function patternMatches(pattern: string, path: string): boolean {
  const value = pattern.trim().replace(/^\.\//, '').replace(/^\/+/, '')
  if (value.endsWith('/')) return segmentsMatch(`${value}**`, path)
  if (!value.includes('/')) {
    const name = path.split('/').pop() ?? path
    return glob(value, name)
  }
  return segmentsMatch(value, path)
}

/**
 * One row of reachable path prefixes per pattern segment, so the work is
 * pattern segments × path segments however many `**` a pattern has.
 */
function segmentsMatch(pattern: string, path: string): boolean {
  const p = pattern.split('/')
  const s = path.split('/')
  // reach[j]: the pattern so far matches the first j path segments.
  const reach: boolean[] = new Array(s.length + 1).fill(false)
  reach[0] = true
  for (const segment of p) {
    if (segment === '**') {
      let any = false
      for (let j = 0; j <= s.length; j++) {
        any = any || reach[j]
        reach[j] = any
      }
    } else {
      for (let j = s.length; j >= 1; j--) {
        reach[j] = reach[j - 1] && glob(segment, s[j - 1])
      }
      reach[0] = false
    }
    if (!reach.includes(true)) return false
  }
  return reach[s.length]
}

/** `*` and `?` within one segment, without backtracking blow-up. */
function glob(pattern: string, text: string): boolean {
  let p = 0
  let t = 0
  let star = -1
  let mark = 0
  while (t < text.length) {
    if (p < pattern.length && (pattern[p] === '?' || pattern[p] === text[t])) {
      p++
      t++
    } else if (p < pattern.length && pattern[p] === '*') {
      star = p
      mark = t
      p++
    } else if (star >= 0) {
      p = star + 1
      mark++
      t = mark
    } else {
      return false
    }
  }
  while (p < pattern.length && pattern[p] === '*') p++
  return p === pattern.length
}

/**
 * What the rules form resets to when the saved rules change: the saved
 * rules while it holds no edit, nothing while it does, so a refetch never
 * discards rules someone is writing.
 */
export function rulesFormReset(
  isDirty: boolean,
  rules: OwnershipRuleView[]
): OwnershipRulesFormData | null {
  return isDirty ? null : rulesToForm(rules)
}

/**
 * Whether the form was changed after `submitted` was sent: then the saved
 * answer becomes the form's baseline but does not replace what is in it,
 * so edits made while a save runs are kept.
 */
export function editedSinceSave(
  current: OwnershipRulesFormData,
  submitted: OwnershipRuleRequest[]
): boolean {
  if (current.rules.length !== submitted.length) return true
  return current.rules.some(
    (rule, i) =>
      rule.pattern.trim() !== submitted[i].pattern ||
      rule.owner_user_id !== submitted[i].owner_user_id
  )
}

/** "acme/shop#12", or the host of a plain link. */
export function issueLabel(link: {
  provider: string
  repository?: string | null
  number?: number | null
  url: string
}): string {
  if (link.provider !== 'link' && link.repository && link.number) {
    return `${link.repository}#${link.number}`
  }
  try {
    return new URL(link.url).host
  } catch {
    return link.url
  }
}

/** The first seven characters of a commit. */
export function shortSha(sha: string): string {
  return sha.slice(0, 7)
}

/** The provider's name as people write it. */
export function providerName(provider: string | null | undefined): string {
  switch (provider) {
    case 'github':
      return 'GitHub'
    case 'gitlab':
      return 'GitLab'
    case 'bitbucket':
      return 'Bitbucket'
    case 'gitea':
      return 'Gitea'
    default:
      return provider ?? 'the repository'
  }
}
