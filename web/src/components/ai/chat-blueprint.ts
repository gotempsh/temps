// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The architecture blueprint of a workspace, as the chat and its working tabs
 * see it: the proposal the assistant made with `temps_propose_blueprint`, the
 * version the user accepted, and which project or service realises each part.
 */

import {
  getBlueprintOptions,
  getBlueprintQueryKey,
  getChatWorkspaceOptions,
  getChatWorkspaceQueryKey,
  listBlueprintVersionsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  BlueprintDocument,
  BlueprintResponse,
  Component,
  ComponentLive,
  LiveStatus,
} from '@/api/client/types.gen'
import { useQuery, type QueryClient } from '@tanstack/react-query'
import { deployOrder } from './blueprint-layout'

/** Tool the assistant calls to propose an architecture. */
export const PROPOSE_BLUEPRINT_TOOL = 'temps_propose_blueprint'

export function isProposeBlueprintTool(name: string): boolean {
  return (
    name === PROPOSE_BLUEPRINT_TOOL ||
    name.endsWith(`__${PROPOSE_BLUEPRINT_TOOL}`)
  )
}

/**
 * Asks the open chat with this id to send `message` as the user, as soon as
 * it is idle. Cards outside the transcript use it to carry the conversation
 * on after the user decides something there.
 */
export const CHAT_CONTINUE_EVENT = 'temps:chat-continue'

export interface ChatContinueDetail {
  conversationPublicId: string
  message: string
}

export function continueChat(conversationPublicId: string, message: string) {
  window.dispatchEvent(
    new CustomEvent<ChatContinueDetail>(CHAT_CONTINUE_EVENT, {
      detail: { conversationPublicId, message },
    })
  )
}

/** Asks the assistant page to start a new chat inside an application. */
export const NEW_CHAT_IN_EVENT = 'temps:new-chat-in'

export interface NewChatInDetail {
  public_id: string
  name: string
}

export function startChatIn(application: NewChatInDetail) {
  window.dispatchEvent(
    new CustomEvent<NewChatInDetail>(NEW_CHAT_IN_EVENT, {
      detail: { public_id: application.public_id, name: application.name },
    })
  )
}

/**
 * Tells the chat list to read itself again: a chat moved between
 * applications, or an application was renamed, so its groups changed.
 */
export const CHAT_LIST_CHANGED_EVENT = 'temps:chat-list-changed'

export function chatListChanged() {
  window.dispatchEvent(new Event(CHAT_LIST_CHANGED_EVENT))
}

/** Asks the assistant page to open another chat. */
export const OPEN_CHAT_EVENT = 'temps:open-chat'

export function openChat(publicId: string) {
  window.dispatchEvent(
    new CustomEvent<{ publicId: string }>(OPEN_CHAT_EVENT, {
      detail: { publicId },
    })
  )
}

/** What `temps_propose_blueprint` returns when it recorded a proposal. */
export interface BlueprintProposalReceipt {
  applicationId: string
  workspaceName: string
  workspaceCreated: boolean
  version: number
  changeSummary: string
  document: BlueprintDocument
}

const KINDS = new Set(['client', 'app', 'data', 'external'])

function isDocument(value: unknown): value is BlueprintDocument {
  if (typeof value !== 'object' || value === null) return false
  const record = value as Record<string, unknown>
  return (
    typeof record.summary === 'string' &&
    Array.isArray(record.components) &&
    record.components.every(
      (component) =>
        typeof component === 'object' &&
        component !== null &&
        typeof (component as Record<string, unknown>).id === 'string' &&
        KINDS.has(String((component as Record<string, unknown>).kind))
    ) &&
    (record.flows === undefined || Array.isArray(record.flows))
  )
}

/**
 * `null` while the call runs and when it did not record a proposal: a
 * rejected design comes back as a sentence the generic tool card shows.
 */
export function parseBlueprintProposal(
  result: string | null | undefined
): BlueprintProposalReceipt | null {
  if (!result) return null
  let value: unknown
  try {
    value = JSON.parse(result)
  } catch {
    return null
  }
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    return null
  }
  const record = value as Record<string, unknown>
  if (record.status !== 'proposed') return null
  if (
    typeof record.application_id !== 'string' ||
    typeof record.version !== 'number' ||
    !isDocument(record.document)
  ) {
    return null
  }
  return {
    applicationId: record.application_id,
    workspaceName:
      typeof record.workspace_name === 'string' ? record.workspace_name : '',
    workspaceCreated: record.workspace_created === true,
    version: record.version,
    changeSummary:
      typeof record.change_summary === 'string' ? record.change_summary : '',
    document: { ...record.document, flows: record.document.flows ?? [] },
  }
}

/** What the chat is told after the user accepts a version. */
export function acceptedMessage(version: number): string {
  return `I accepted the architecture (version ${version}). Build it as designed.`
}

/** What the chat is told after the user rejects a version. */
export function rejectedMessage(version: number, reason: string): string {
  const why = reason.trim()
  return why
    ? `I rejected the proposed architecture (version ${version}). ${why}`
    : `I rejected the proposed architecture (version ${version}). Ask me what to change before proposing again.`
}

/** What the chat is told after the user edits the design themselves. */
export function editedMessage(version: number, summary: string): string {
  return `I changed the architecture myself (version ${version}): ${summary.trim()} Build against the new version.`
}

/**
 * "Deploy all": the app parts, bound or not, in the order they should go out,
 * so an API is live before the front end that calls it.
 */
export function deployAllMessage(document: BlueprintDocument): string | null {
  const order = deployOrder(document.components, document.flows ?? [])
  if (order.length === 0) return null
  const names = order
    .map((id) => {
      const part = document.components.find((c) => c.id === id)
      return part ? `${part.name} (\`${id}\`)` : `\`${id}\``
    })
    .join(', then ')
  return `Deploy every app part of the accepted architecture from the sandbox, in this order: ${names}. Create the projects and services that are not bound yet, bind each one, and tell me the URLs once they are live.`
}

const SETTLING: ReadonlySet<LiveStatus> = new Set(['created', 'building'])

export function isSettling(live: Record<string, ComponentLive>): boolean {
  return Object.values(live).some((part) => SETTLING.has(part.status))
}

/** The chat's workspace (or why it has none), and its blueprint. */
export function useChatBlueprint(conversationPublicId: string) {
  const workspace = useQuery(
    getChatWorkspaceOptions({ path: { public_id: conversationPublicId } })
  )
  const applicationId = workspace.data?.workspace?.public_id ?? null
  const blueprint = useQuery({
    ...getBlueprintOptions({ path: { public_id: applicationId ?? '' } }),
    enabled: applicationId !== null,
    // Keep checking while a bound part is still building.
    refetchInterval: (query) =>
      query.state.data && isSettling(query.state.data.live) ? 5000 : false,
  })
  return { workspace, applicationId, blueprint }
}

/** Refresh everything that shows a workspace's blueprint. */
export function invalidateBlueprint(
  queryClient: QueryClient,
  applicationId: string,
  conversationPublicId?: string
) {
  void queryClient.invalidateQueries({
    queryKey: getBlueprintQueryKey({ path: { public_id: applicationId } }),
  })
  void queryClient.invalidateQueries({
    queryKey: listBlueprintVersionsQueryKey({
      path: { public_id: applicationId },
    }),
  })
  if (conversationPublicId) {
    void queryClient.invalidateQueries({
      queryKey: getChatWorkspaceQueryKey({
        path: { public_id: conversationPublicId },
      }),
    })
  }
}

/** Put a fresh blueprint straight into the cache after a mutation. */
export function storeBlueprint(
  queryClient: QueryClient,
  blueprint: BlueprintResponse
) {
  queryClient.setQueryData(
    getBlueprintQueryKey({ path: { public_id: blueprint.application_id } }),
    blueprint
  )
}

/** Whether a part is something Temps deploys or provisions. */
export function isDeployable(component: Component): boolean {
  return component.kind === 'app' || component.kind === 'data'
}

/** Human labels for live states. */
export const LIVE_LABELS: Record<LiveStatus, string> = {
  created: 'Not deployed',
  building: 'Building',
  live: 'Live',
  failed: 'Failed',
  stopped: 'Stopped',
  missing: 'Missing',
}

export const LIVE_TONES: Record<LiveStatus, string> = {
  created: 'bg-muted text-muted-foreground',
  building: 'bg-amber-500/15 text-amber-700 dark:text-amber-300',
  live: 'bg-emerald-500/15 text-emerald-700 dark:text-emerald-300',
  failed: 'bg-destructive/15 text-destructive',
  stopped: 'bg-muted text-muted-foreground',
  missing: 'bg-destructive/15 text-destructive',
}

/** The second line under a part's name: what it is built with. */
export function componentDetail(component: Component): string {
  switch (component.kind) {
    case 'client':
      return component.surface?.replace('_', ' ') ?? 'client'
    case 'app':
      return [component.role, component.stack ?? component.image]
        .filter(Boolean)
        .join(' · ')
    case 'data':
      return [component.engine, component.holds?.slice(0, 2).join(', ')]
        .filter(Boolean)
        .join(' · ')
    case 'external':
      return component.provider ?? 'outside service'
  }
}

export type ProposalState =
  'open' | 'accepted' | 'rejected' | 'superseded' | 'unknown'

/** Where this proposal stands now, from the workspace's current blueprint. */
export function proposalState(
  blueprint: BlueprintResponse | undefined,
  version: number
): ProposalState {
  if (!blueprint) return 'unknown'
  if (blueprint.proposal?.version === version) return 'open'
  if (blueprint.accepted?.version === version) return 'accepted'
  const newest = Math.max(
    blueprint.proposal?.version ?? 0,
    blueprint.accepted?.version ?? 0
  )
  return newest > version ? 'superseded' : 'rejected'
}
