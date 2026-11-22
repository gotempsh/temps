// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Button } from '@/components/ui/button'
import { ListPlus, Paperclip, Pencil, Send, X } from 'lucide-react'
import type { QueuedChatMessage } from './chat-message-queue'

/**
 * Follow-ups the user queued while a turn ran. They go out together, as one
 * message, when the turn finishes; until then each can be taken back to edit
 * or dropped, and the whole queue can be sent at once.
 */
export function QueuedMessages({
  messages,
  waiting,
  onEdit,
  onRemove,
  onSendNow,
}: {
  messages: readonly QueuedChatMessage[]
  /** A turn is running, so the queue is waiting for it. */
  waiting: boolean
  onEdit: (message: QueuedChatMessage) => void
  onRemove: (id: string) => void
  onSendNow: () => void
}) {
  if (messages.length === 0) return null
  return (
    <section
      aria-label="Queued messages"
      className="space-y-1.5 border-b px-3 py-2"
    >
      <div className="flex items-center gap-2">
        <ListPlus
          className="size-3.5 shrink-0 text-muted-foreground"
          aria-hidden="true"
        />
        <p className="min-w-0 flex-1 text-xs text-muted-foreground">
          {waiting
            ? `${messages.length === 1 ? 'Sends' : `${messages.length} messages send together`} when this turn finishes`
            : 'Not sent yet: the last turn did not finish cleanly'}
        </p>
        <Button
          type="button"
          size="sm"
          variant="ghost"
          className="h-6 gap-1 px-2 text-xs"
          onClick={onSendNow}
          title={
            waiting
              ? 'Interrupt the running turn and send these now'
              : 'Send these now'
          }
        >
          <Send className="size-3" aria-hidden="true" />
          Send now
        </Button>
      </div>
      <ul className="space-y-1">
        {messages.map((message) => (
          <li
            key={message.id}
            className="group flex items-start gap-2 rounded-md bg-muted/60 px-2 py-1.5 text-sm"
          >
            <p className="line-clamp-2 min-w-0 flex-1 whitespace-pre-wrap break-words">
              {message.text || (
                <span className="text-muted-foreground">Files only</span>
              )}
            </p>
            {message.attachments.length > 0 && (
              <span
                className="flex shrink-0 items-center gap-1 text-xs text-muted-foreground"
                title={message.attachments.map((file) => file.name).join(', ')}
              >
                <Paperclip className="size-3" aria-hidden="true" />
                {message.attachments.length}
              </span>
            )}
            <Button
              type="button"
              size="icon"
              variant="ghost"
              className="size-6 shrink-0"
              onClick={() => onEdit(message)}
              title="Edit: move back into the message box"
              aria-label="Edit queued message"
            >
              <Pencil className="size-3" />
            </Button>
            <Button
              type="button"
              size="icon"
              variant="ghost"
              className="size-6 shrink-0"
              onClick={() => onRemove(message.id)}
              title="Remove from the queue"
              aria-label="Remove queued message"
            >
              <X className="size-3" />
            </Button>
          </li>
        ))}
      </ul>
    </section>
  )
}
