// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'
import { asksBeforeChanges, permissionModeForAsk } from './assistant-state'

/**
 * The assistant's one write-safety control, kept visible in Simple mode where
 * the runtime pickers are hidden. Off (the default) applies changes straight
 * away; on makes each one wait for the user's approval. Deletions always wait,
 * whichever way this is set.
 */
export function AskBeforeChangesToggle({
  permissionModeId,
  disabled,
  onChange,
}: {
  permissionModeId: string | null
  disabled?: boolean
  onChange: (permissionModeId: string) => void
}) {
  return (
    <div
      className="flex h-8 items-center gap-2 px-2"
      title="Off: changes run straight away. On: each change waits for your approval. Deletions always ask."
    >
      <Switch
        id="assistant-ask-before-changes"
        checked={asksBeforeChanges(permissionModeId)}
        disabled={disabled}
        onCheckedChange={(ask) => onChange(permissionModeForAsk(ask))}
      />
      <Label
        htmlFor="assistant-ask-before-changes"
        className="cursor-pointer text-xs font-normal text-muted-foreground"
      >
        <span className="@[40rem]:hidden">Ask first</span>
        <span className="hidden @[40rem]:inline">Ask before changes</span>
      </Label>
    </div>
  )
}
