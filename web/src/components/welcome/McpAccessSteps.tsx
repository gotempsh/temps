// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { CopyButton } from '@/components/ui/copy-button'
import { problemDetail } from '@/lib/api-problem'
import type { McpKeyAccess } from '@/lib/mcp-clients'
import { Button } from '@temps-sdk/ds'
import { Check } from 'lucide-react'
import { toast } from 'sonner'
import { type Choice, ChoiceList } from './ChoiceList'
import type { McpAccess } from './useMcpAccess'

const KEY_ACCESS: Choice<McpKeyAccess>[] = [
  { id: 'changes', label: 'Can make changes' },
  { id: 'read', label: 'Read only' },
]

const KEY_ACCESS_DETAIL: Record<McpKeyAccess, string> = {
  changes:
    'Create projects, deploy, set variables. Each change waits for your approval.',
  read: 'Projects, deployments, logs and errors. Changes are refused.',
}

export function McpServerStep({ mcp }: { mcp: McpAccess }) {
  return (
    <li className="space-y-2">
      <p className="font-medium">1. Turn on the MCP server</p>
      {mcp.mcpState === 'unknown' ? (
        <p className="text-muted-foreground">
          An admin turns it on under Settings, MCP server.
        </p>
      ) : mcp.mcpState === 'loading' ? (
        <p className="text-muted-foreground">Checking…</p>
      ) : mcp.mcpState === 'on' ? (
        <p className="inline-flex items-center gap-1.5 text-muted-foreground">
          <Check className="size-4 text-success" aria-hidden />
          The MCP server is on.
        </p>
      ) : (
        <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
          <p className="text-muted-foreground sm:flex-1">
            It is off, so nothing can connect to it yet.
          </p>
          <Button
            variant="outline"
            busy={mcp.turnOn.isPending}
            busyLabel="Turning on…"
            onClick={() =>
              mcp.turnOn.mutate(
                { mcp_server: { enabled: true } },
                { onSuccess: () => toast.success('MCP server turned on') }
              )
            }
          >
            Turn on
          </Button>
        </div>
      )}
    </li>
  )
}

export function McpKeyStep({
  mcp,
  pasteHint,
}: {
  mcp: McpAccess
  /** Where the key goes, for the line under it. */
  pasteHint: string
}) {
  return (
    <li className="space-y-2">
      <p className="font-medium">2. Create a key</p>
      {mcp.apiKey ? (
        <div className="space-y-1.5">
          <div className="flex items-center gap-2 rounded-md border bg-muted/40 px-3 py-2">
            <code className="min-w-0 flex-1 font-mono text-xs [overflow-wrap:anywhere]">
              {mcp.apiKey}
            </code>
            <CopyButton value={mcp.apiKey} />
          </div>
          <p className="text-xs text-muted-foreground">
            Shown once. {pasteHint} Revoke it any time under Settings, API keys.
          </p>
        </div>
      ) : (
        <div className="space-y-3">
          <div className="space-y-1.5 sm:max-w-md">
            <ChoiceList
              label="What the key may do"
              choices={KEY_ACCESS}
              value={mcp.access}
              onChange={mcp.setAccess}
              columns={2}
              size="compact"
            />
            <p className="text-xs text-muted-foreground">
              {KEY_ACCESS_DETAIL[mcp.access]}
            </p>
          </div>
          <Button
            variant="outline"
            busy={mcp.createKey.isPending}
            busyLabel="Creating…"
            onClick={mcp.create}
          >
            Create key
          </Button>
        </div>
      )}
    </li>
  )
}

/** The error of either step, if any, and the step-up dialog. */
export function McpAccessFooter({ mcp }: { mcp: McpAccess }) {
  return (
    <>
      {mcp.error && (
        <p className="text-sm text-destructive" role="status">
          {problemDetail(mcp.error, 'Something went wrong. Try again.')}
        </p>
      )}
      {mcp.verificationDialog}
    </>
  )
}
