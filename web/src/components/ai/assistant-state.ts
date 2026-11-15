// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { isInternalConsolePath } from '@/lib/cloud-telemetry-activation'

/** `context_type` of conversations started from the Temps assistant. */
export const ASSISTANT_CONTEXT_TYPE = 'assistant'

/**
 * Where an operator pastes the one key the assistant needs. The query opens the
 * add-key dialog on the AI Gateway page with OpenRouter preselected.
 */
export const OPENROUTER_SETUP_PATH = '/ai-gateway?add=openrouter'

export const NO_PROVIDER_REASON =
  'No AI provider is configured. Add an OpenRouter API key to start managing Temps by chat.'

/** Gateway permission mode that has every change wait for the user's approval. */
export const ASK_BEFORE_CHANGES_MODE = 'confirm-actions'

/**
 * Gateway permission mode that applies changes without asking. Deletions still
 * wait for approval: the server never auto-approves them.
 */
export const RUN_CHANGES_AUTOMATICALLY_MODE = 'auto'

/** Whether a conversation's permission mode makes changes wait for approval. */
export function asksBeforeChanges(permissionModeId: string | null): boolean {
  return permissionModeId === ASK_BEFORE_CHANGES_MODE
}

export function permissionModeForAsk(ask: boolean): string {
  return ask ? ASK_BEFORE_CHANGES_MODE : RUN_CHANGES_AUTOMATICALLY_MODE
}

/**
 * Things the assistant can finish on its own, shown before the first message.
 * Each is a complete goal that needs nothing the user may not have yet (a
 * repository, a database, a failed deployment): the assistant writes the app
 * in a sandbox, adds the services it needs and deploys it, so the user ends
 * with a live URL and sees what Temps does.
 */
export const ASSISTANT_EXAMPLE_PROMPTS: readonly string[] = [
  'Build a calculator web app and deploy it',
  'Build a to-do list app that saves tasks in Postgres and deploy it',
  'Build a page with a visit counter stored in Redis and deploy it',
  'Deploy a public Git repository and give me its URL',
]

const LAST_MODEL_KEY = 'temps.assistant.last-model'

/** The model the user last started or switched a chat to, if any. */
export function loadLastAssistantModel(): string | null {
  try {
    return window.localStorage.getItem(LAST_MODEL_KEY)
  } catch {
    return null
  }
}

/**
 * Remember a model so the next new chat starts on it. This is only a starting
 * point for new chats: every chat keeps the model it was given.
 */
export function saveLastAssistantModel(modelId: string): void {
  try {
    window.localStorage.setItem(LAST_MODEL_KEY, modelId)
  } catch {
    // Storage can be unavailable (private mode); the default still applies.
  }
}

const LAST_KEY_KEY = 'temps.assistant.last-key'

/** The provider key the user last started a chat on, if any. */
export function loadLastAssistantKeyId(): number | null {
  try {
    const stored = Number(window.localStorage.getItem(LAST_KEY_KEY))
    return Number.isInteger(stored) && stored > 0 ? stored : null
  } catch {
    return null
  }
}

/**
 * Remember the provider key a new chat was started on, so the next one starts
 * there too. Like the model, only a starting point for new chats.
 */
export function saveLastAssistantKeyId(keyId: number): void {
  try {
    window.localStorage.setItem(LAST_KEY_KEY, String(keyId))
  } catch {
    // Storage can be unavailable (private mode); the default key still applies.
  }
}

export function isAssistantConversation(conversation: {
  context_type: string
}): boolean {
  return conversation.context_type === ASSISTANT_CONTEXT_TYPE
}

/**
 * Whether a chat belongs to the user rather than to a project: the assistant,
 * and the application and global chats started on the retired AI workspace
 * page. They all open through the user-rooted routes.
 */
export function isUserRootedConversation(conversation: {
  context_type: string
}): boolean {
  return (
    isAssistantConversation(conversation) ||
    conversation.context_type === 'application' ||
    conversation.context_type === 'global'
  )
}

/** The fields of the provider-status capability response the assistant reads. */
export interface AssistantCapabilityInput {
  gateway_available: boolean
  reason?: string | null
  setup_path?: string | null
}

export type AssistantReadiness =
  | { state: 'ready' }
  | { state: 'unconfigured'; reason: string; setupPath: string }

/**
 * Turn the capability response into what the UI renders. The assistant runs on
 * an AI gateway provider, so a host harness being ready does not count.
 *
 * Always yields a reason and a console path when unconfigured: a server that
 * omits them, or sends an off-console `setup_path`, must not leave the user
 * with a bare disabled control.
 */
export function assistantReadiness(
  status: AssistantCapabilityInput | undefined
): AssistantReadiness {
  if (status?.gateway_available) return { state: 'ready' }
  return { state: 'unconfigured', ...unconfiguredAssistant(status) }
}

/** The reason and fix to show when there is no usable provider. */
export function unconfiguredAssistant(
  status: Partial<AssistantCapabilityInput> | undefined
): { reason: string; setupPath: string } {
  return {
    reason: status?.reason?.trim() || NO_PROVIDER_REASON,
    setupPath: isInternalConsolePath(status?.setup_path)
      ? status.setup_path
      : OPENROUTER_SETUP_PATH,
  }
}

/** Pull the server's `setup_path` out of a 409 problem document, if it sent one. */
export function setupPathFromProblem(problem: unknown): string | undefined {
  if (typeof problem !== 'object' || problem === null) return undefined
  const path = (problem as { setup_path?: unknown }).setup_path
  return isInternalConsolePath(typeof path === 'string' ? path : undefined)
    ? (path as string)
    : undefined
}

/** Tile and label colours for a chat in the list. Full class names, so the
 * Tailwind scanner keeps every one of them. */
export interface ChatTint {
  tile: string
  label: string
}

const CHAT_TINTS: readonly ChatTint[] = [
  {
    tile: 'bg-violet-500/15 text-violet-600 dark:text-violet-300',
    label: 'bg-violet-500/10 text-violet-700 dark:text-violet-300',
  },
  {
    tile: 'bg-sky-500/15 text-sky-600 dark:text-sky-300',
    label: 'bg-sky-500/10 text-sky-700 dark:text-sky-300',
  },
  {
    tile: 'bg-emerald-500/15 text-emerald-600 dark:text-emerald-300',
    label: 'bg-emerald-500/10 text-emerald-700 dark:text-emerald-300',
  },
  {
    tile: 'bg-amber-500/15 text-amber-600 dark:text-amber-300',
    label: 'bg-amber-500/10 text-amber-700 dark:text-amber-300',
  },
  {
    tile: 'bg-rose-500/15 text-rose-600 dark:text-rose-300',
    label: 'bg-rose-500/10 text-rose-700 dark:text-rose-300',
  },
  {
    tile: 'bg-cyan-500/15 text-cyan-600 dark:text-cyan-300',
    label: 'bg-cyan-500/10 text-cyan-700 dark:text-cyan-300',
  },
]

/** A stable colour per chat, so a chat keeps its look across reloads and the
 * list is scannable without reading every title. */
export function chatTint(seed: string): ChatTint {
  let hash = 0
  for (let i = 0; i < seed.length; i++) {
    hash = (hash * 31 + seed.charCodeAt(i)) >>> 0
  }
  return CHAT_TINTS[hash % CHAT_TINTS.length]
}
