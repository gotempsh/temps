// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandSeparator,
} from '@/components/ui/command'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { cn } from '@/lib/utils'
import { Check, FolderGit2, Plus, Sparkles } from 'lucide-react'

export interface SwitcherChat {
  id: string
  title: string
  /** What kind of chat it is, or the project it is about. */
  subtitle: string
  lastActivityAt: string
}

/**
 * Jump to another chat, or start one, without the chat list on screen. Opened
 * from the page header or with ⌘K while the chat list is hidden.
 *
 * `chats` are the ones already loaded and are filtered as you type. The list
 * only holds the most recent pages, so the typed text also goes out through
 * `onSearchChange`, and matches from every chat come back as `moreChats`.
 */
export function AgentSwitcher({
  open,
  onOpenChange,
  chats,
  search,
  onSearchChange,
  moreChats,
  searching,
  activeId,
  onOpen,
  onNewAssistant,
  onNewProjectChat,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  chats: SwitcherChat[]
  search: string
  onSearchChange: (search: string) => void
  /** Chats matching `search` that are not in `chats`. */
  moreChats: SwitcherChat[]
  /** The search over every chat is still running. */
  searching: boolean
  activeId: string | null
  onOpen: (id: string) => void
  onNewAssistant: () => void
  onNewProjectChat: () => void
}) {
  const choose = (action: () => void) => () => {
    onOpenChange(false)
    action()
  }
  const renderChat = (chat: SwitcherChat) => (
    <CommandItem
      key={chat.id}
      value={`${chat.title} ${chat.subtitle} ${chat.id}`}
      onSelect={choose(() => onOpen(chat.id))}
    >
      {chat.subtitle === 'Assistant' ? (
        <Sparkles className="mr-2 shrink-0" />
      ) : (
        <FolderGit2 className="mr-2 shrink-0" />
      )}
      <span className="min-w-0 flex-1 truncate">{chat.title}</span>
      <span className="ml-3 hidden shrink-0 truncate text-xs text-muted-foreground sm:inline">
        {chat.subtitle}
      </span>
      <span className="ml-3 shrink-0 text-xs text-muted-foreground">
        <TimeAgo date={chat.lastActivityAt} />
      </span>
      <Check
        className={cn(
          'ml-2 shrink-0',
          chat.id === activeId ? 'opacity-100' : 'opacity-0'
        )}
      />
    </CommandItem>
  )
  return (
    <CommandDialog open={open} onOpenChange={onOpenChange}>
      <CommandInput
        placeholder="Switch chat or start a new one…"
        value={search}
        onValueChange={onSearchChange}
      />
      <CommandList>
        <CommandEmpty>
          {searching
            ? 'Searching all your chats…'
            : 'No chat matches. Try another word.'}
        </CommandEmpty>
        <CommandGroup heading="Start">
          <CommandItem
            value="new assistant chat ask temps"
            onSelect={choose(onNewAssistant)}
          >
            <Sparkles className="mr-2" />
            New chat with Temps
          </CommandItem>
          <CommandItem
            value="new project chat"
            onSelect={choose(onNewProjectChat)}
          >
            <Plus className="mr-2" />
            New chat about a project…
          </CommandItem>
        </CommandGroup>
        {chats.length > 0 && <CommandSeparator />}
        {chats.length > 0 && (
          <CommandGroup heading="Chats">{chats.map(renderChat)}</CommandGroup>
        )}
        {moreChats.length > 0 && (
          <CommandGroup heading="Older chats">
            {moreChats.map(renderChat)}
          </CommandGroup>
        )}
      </CommandList>
    </CommandDialog>
  )
}
