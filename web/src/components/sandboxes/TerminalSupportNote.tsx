// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { TerminalCapability } from '@/api/client'
import { CopyButton } from '@/components/ui/copy-button'

export type { TerminalCapability }

interface TerminalSupportNoteProps {
  /** `undefined` while the status query is loading. */
  capability: TerminalCapability | undefined
}

/**
 * Says up front that `temps sandbox shell` won't work on a backend, and what
 * to run to fix it. Renders nothing while loading or when terminals work, so
 * a ready backend stays uncluttered — the note exists to replace the bare
 * 409 someone would otherwise hit when opening a terminal.
 */
export function TerminalSupportNote({ capability }: TerminalSupportNoteProps) {
  if (capability == null || capability.supported) {
    return null
  }
  return (
    <div className="rounded-md border border-dashed p-2 text-xs text-muted-foreground space-y-1.5">
      <p>
        <span className="font-medium text-foreground">
          Terminals unavailable:
        </span>{' '}
        {capability.reason ?? 'the guest terminal agent is not set up.'}
      </p>
      {capability.setup_command && (
        <div className="flex items-center gap-2">
          <code className="rounded bg-muted px-1.5 py-0.5 font-mono">
            {capability.setup_command}
          </code>
          <CopyButton
            value={capability.setup_command}
            className="h-6 w-6"
            label="Copy terminal setup command"
          />
        </div>
      )}
    </div>
  )
}
