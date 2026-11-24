// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Where a chat's latest turn stands, as the chat list shows it. */
export type ChatRunState =
  'working' | 'finished' | 'error' | 'stopped' | 'interrupted'

/**
 * The state of a chat's latest turn, from the server's `turn_status`. A chat
 * that never ran a turn (`idle`) has none, so the list shows nothing for it.
 */
export function chatRunState(turnStatus: string): ChatRunState | null {
  switch (turnStatus) {
    case 'running':
      return 'working'
    case 'completed':
      return 'finished'
    case 'failed':
      return 'error'
    case 'cancelled':
      return 'stopped'
    case 'interrupted':
      return 'interrupted'
    default:
      return null
  }
}

export interface ChatRunPresentation {
  label: string
  /** Shown on hover: what the state means, and the error when there is one. */
  title: string
}

export function chatRunPresentation(
  state: ChatRunState,
  failureTitle?: string | null
): ChatRunPresentation {
  switch (state) {
    case 'working':
      return {
        label: 'Working',
        title: 'The assistant is working on this chat',
      }
    case 'finished':
      return { label: 'Finished', title: 'The last answer is complete' }
    case 'error':
      return {
        label: 'Error',
        title: failureTitle
          ? `The last turn failed: ${failureTitle}`
          : 'The last turn failed',
      }
    case 'stopped':
      return { label: 'Stopped', title: 'The last turn was stopped' }
    case 'interrupted':
      return {
        label: 'Interrupted',
        title:
          'Temps restarted while the assistant was working; send a message to carry on',
      }
  }
}

/** Whether any listed chat is still working, so the list keeps checking. */
export function anyChatWorking(
  chats: readonly { turn_status: string }[]
): boolean {
  return chats.some((chat) => chat.turn_status === 'running')
}

/** How often the list asks again while a chat is working. */
export const WORKING_CHAT_POLL_MS = 5000

/**
 * How often it asks otherwise: a chat can start working without this tab
 * sending anything (a goal brings it back, another tab sends a message).
 */
export const IDLE_CHAT_POLL_MS = 30_000

export function chatListPollInterval(
  chats: readonly { turn_status: string }[]
): number {
  return anyChatWorking(chats) ? WORKING_CHAT_POLL_MS : IDLE_CHAT_POLL_MS
}

/**
 * The list with one chat's turn status replaced. The same array comes back
 * when nothing changes, so a repeated announcement does not re-render.
 */
export function withTurnStatus<
  T extends { public_id: string; turn_status: string },
>(chats: T[], publicId: string, turnStatus: string): T[] {
  const index = chats.findIndex((chat) => chat.public_id === publicId)
  if (index === -1 || chats[index].turn_status === turnStatus) return chats
  const next = [...chats]
  next[index] = { ...chats[index], turn_status: turnStatus }
  return next
}

/** The open chat telling the list its turn started or ended. */
export const CHAT_TURN_EVENT = 'temps:chat-turn'

export interface ChatTurnDetail {
  publicId: string
  active: boolean
}

export function announceChatTurn(publicId: string, active: boolean) {
  window.dispatchEvent(
    new CustomEvent<ChatTurnDetail>(CHAT_TURN_EVENT, {
      detail: { publicId, active },
    })
  )
}

/**
 * Whether the collapsed chat rail marks a chat in this state. It marks what
 * needs a look (still working, failed, cut short) and leaves finished and
 * stopped chats plain, so a rail of done chats stays quiet.
 */
export function railMarksState(state: ChatRunState | null): boolean {
  return state === 'working' || state === 'error' || state === 'interrupted'
}
