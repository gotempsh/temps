// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * What a workspace thread is: a conversation with an AI harness, or an
 * interactive terminal in the workspace sandbox. Fixed when the thread is
 * created; `ConversationResponse.kind` carries it.
 */
export type ThreadKind = 'ai' | 'terminal'

export const THREAD_KINDS: ReadonlyArray<{
  kind: ThreadKind
  label: string
  description: string
}> = [
  {
    kind: 'ai',
    label: 'AI thread',
    description:
      'Chat with Claude Code, Codex or OpenCode. It reads and edits the workspace and can use Temps tools.',
  },
  {
    kind: 'terminal',
    label: 'Terminal',
    description:
      'A shell in the workspace sandbox. Run commands yourself; it keeps running when you close the tab.',
  },
]

/**
 * The kind of a thread as listed by the API. A thread from a server that
 * predates kinds has none, and every such thread is an AI thread.
 */
export function threadKind(
  thread: { kind?: string | null } | null | undefined
): ThreadKind {
  return thread?.kind === 'terminal' ? 'terminal' : 'ai'
}

export function isTerminalThread(
  thread: { kind?: string | null } | null | undefined
): boolean {
  return threadKind(thread) === 'terminal'
}

/** Secondary line of a thread row: what runs in it. */
export function terminalThreadSubtitle(archived: boolean): string {
  return archived ? 'Terminal · archived' : 'Terminal · workspace shell'
}
