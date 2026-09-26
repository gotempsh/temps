// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ConversationResponse, SendMessageRequest } from '@/api/client'
import type { ChatRuntimeSelection } from '@/components/ai/chat-runtime-options'

/** Keep the accepted thread and immutable turn request together across retries. */
export function workspaceFirstTask() {
  let conversation: ConversationResponse | null = null
  let attempted = false
  let request: SendMessageRequest | null = null
  return {
    async start({
      prompt,
      selection,
      createThread,
      recoverThread,
      send,
      turnId,
    }: {
      prompt: string
      selection: ChatRuntimeSelection
      createThread: () => Promise<ConversationResponse>
      recoverThread: () => Promise<ConversationResponse | null>
      send: (
        conversationId: string,
        request: SendMessageRequest
      ) => Promise<unknown>
      turnId: () => string
    }) {
      if (!prompt.trim())
        throw new Error(
          'Describe your first task before starting the workspace.'
        )
      if (!conversation && attempted) {
        conversation = await recoverThread()
        if (!conversation)
          throw new Error(
            'Could not confirm whether the first thread was created. Open the workspace to inspect it before creating another thread.'
          )
      }
      if (!conversation) {
        attempted = true
        conversation = await createThread()
      }
      request ??= {
        content: prompt.trim(),
        turn_id: turnId(),
        ai_model: selection.modelId,
        ai_thinking_level: selection.thinkingOptionId,
        ai_permission_mode: selection.permissionModeId,
      }
      await send(conversation.public_id, request)
      return conversation
    },
  }
}
