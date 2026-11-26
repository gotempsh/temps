// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Shield } from 'lucide-react'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import {
  ASK_BEFORE_CHANGES_MODE,
  ASSISTANT_PERMISSION_CHOICES,
  RUN_CHANGES_AUTOMATICALLY_MODE,
  asksBeforeChanges,
} from './assistant-state'

/**
 * The assistant's one write-safety control, kept visible in Simple mode where
 * the runtime pickers are hidden: "Ask" makes each change wait for the user's
 * approval, "Full permission" applies changes straight away. Deletions always
 * wait, whichever is chosen.
 */
export function AssistantPermissionSelect({
  permissionModeId,
  disabled,
  onChange,
}: {
  permissionModeId: string | null
  disabled?: boolean
  onChange: (permissionModeId: string) => void
}) {
  return (
    <Select
      value={
        asksBeforeChanges(permissionModeId)
          ? ASK_BEFORE_CHANGES_MODE
          : RUN_CHANGES_AUTOMATICALLY_MODE
      }
      onValueChange={onChange}
      disabled={disabled}
    >
      <SelectTrigger
        aria-label="Permission mode"
        className="h-8 w-auto gap-1 rounded-full border-0 bg-transparent px-2 text-xs shadow-none hover:bg-muted"
        title="Ask: each change waits for your approval. Full permission: changes run straight away. Deletions always ask."
      >
        <Shield className="size-3.5 shrink-0" aria-hidden="true" />
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        {ASSISTANT_PERMISSION_CHOICES.map((choice) => (
          <SelectItem key={choice.id} value={choice.id}>
            {choice.label}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  )
}
