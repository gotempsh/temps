// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { MicrosandboxCapability } from '@/api/client'
import { Badge } from '@/components/ui/badge'
import { CopyButton } from '@/components/ui/copy-button'

export type { MicrosandboxCapability }

interface MicrosandboxBackendOptionProps {
  /** `undefined` while the status query is loading. */
  capability: MicrosandboxCapability | undefined
  selected: boolean
  onSelect: () => void
}

/**
 * The microsandbox choice in the isolation-backend picker. Always rendered:
 * on a host that can't run it yet, it explains exactly what is missing and
 * shows the command that installs it, instead of disappearing.
 */
export function MicrosandboxBackendOption({
  capability,
  selected,
  onSelect,
}: MicrosandboxBackendOptionProps) {
  const unavailable = capability != null && !capability.configured

  return (
    <div className="space-y-2">
      <button
        type="button"
        onClick={() => {
          if (!unavailable) onSelect()
        }}
        disabled={unavailable}
        className={`w-full rounded-lg border p-3 text-left transition-colors ${
          selected
            ? 'border-primary bg-primary/5'
            : 'border-border hover:border-primary/50'
        } disabled:opacity-50 disabled:cursor-not-allowed`}
      >
        <div className="flex items-center gap-2">
          <p className="text-sm font-medium">microsandbox</p>
          <Badge variant="outline" className="text-[10px] px-1.5 py-0">
            Experimental
          </Badge>
        </div>
        <p className="text-xs text-muted-foreground">
          libkrun microVMs with native image pulls and host-side network policy.
          Runs on Linux with KVM and on Apple Silicon Macs. For trusted and
          agent workloads — use Firecracker for hostile multi-tenant code.
        </p>
      </button>
      {unavailable && (
        <div className="rounded-md border border-dashed p-2 text-xs text-muted-foreground space-y-1.5">
          <p>
            <span className="font-medium text-foreground">
              Not available on this host:
            </span>{' '}
            {capability.reason}
          </p>
          {capability.setup_command && (
            <div className="flex items-center gap-2">
              <code className="rounded bg-muted px-1.5 py-0.5 font-mono">
                {capability.setup_command}
              </code>
              <CopyButton
                value={capability.setup_command}
                className="h-6 w-6"
                label="Copy setup command"
              />
            </div>
          )}
          {capability.setup_command && (
            <p>
              Run it on the server, then restart temps to enable the backend.
            </p>
          )}
        </div>
      )}
    </div>
  )
}
