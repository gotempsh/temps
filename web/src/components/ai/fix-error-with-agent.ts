// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { OpenOptions } from './AiAssistantContext-shared'

/** The parts of an error group the agent needs to start on a fix. */
export interface FixableError {
  id: number
  title: string
  error_type?: string | null
  environment_name?: string | null
  project: { id: number; slug: string; name: string }
}

/**
 * Opens Agents on one error, in the error's own project.
 *
 * The thread is a project chat, so the agent gets that project's context and
 * the `temps` tool scoped to it, and can read the group's stack trace and
 * events itself. The context id is per error group: the same error reopens
 * its thread, and two errors never share one.
 */
export function fixErrorWithAgentOptions(error: FixableError): OpenOptions {
  const type = error.error_type?.trim()
  const title = error.title.trim()
  const what =
    type && !title.startsWith(`${type}: `) ? `${type}: ${title}` : title
  const where = error.environment_name
    ? `${error.project.name} (${error.environment_name})`
    : error.project.name
  return {
    projectId: error.project.id,
    context: {
      contextType: 'project',
      contextId: `error-group-${error.id}`,
      title: `Fix error #${error.id}`,
      description: `The agent reads error #${error.id}'s stack trace and recent events in ${error.project.name}, finds the root cause and proposes a fix.`,
      startPrompt: `Find the root cause of error #${error.id} in ${where} and propose a fix.\n\nError: ${what}\n\nStart from its stack trace and most recent events, then point to the code that has to change.`,
      projectSlug: error.project.slug,
      projectName: error.project.name,
    },
  }
}

/**
 * Whether an error list row offers "Fix with agent". Resolved errors need no
 * fix, and ignored ones were set aside on purpose, so repeating the action on
 * them only buries the rows that need it. The error's own page still offers
 * it on an ignored error, where someone went looking.
 */
export function offersFixInList(status: string | null | undefined) {
  return status !== 'resolved' && status !== 'ignored'
}
