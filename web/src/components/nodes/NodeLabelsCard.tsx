// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Tag } from 'lucide-react'

import { Badge } from '@/components/ui/badge'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { CopyButton } from '@/components/ui/copy-button'
import { nodeRoleChangeCommand } from '@/lib/node-role'

function labelEntries(labels: unknown): Array<[string, unknown]> {
  if (!labels || typeof labels !== 'object' || Array.isArray(labels)) return []
  return Object.entries(labels as Record<string, unknown>)
}

/**
 * A node's scheduling labels, and how to change them. Labels are reported
 * by the worker itself (ADR-091), so the card shows the command to run on
 * the worker rather than an editor; it is shown even with no labels, so an
 * operator looking for "make this node dedicated" finds it here.
 */
export function NodeLabelsCard({
  labels,
  showChangeHint,
}: {
  labels: unknown
  /** Workers report their own labels; other node kinds get no hint. */
  showChangeHint: boolean
}) {
  const entries = labelEntries(labels)
  if (entries.length === 0 && !showChangeHint) return null
  const command = nodeRoleChangeCommand(labels)

  return (
    <Card>
      <CardHeader className="py-3 px-4">
        <CardTitle className="text-sm flex items-center gap-2">
          <Tag className="h-4 w-4" />
          Labels
        </CardTitle>
      </CardHeader>
      <CardContent className="px-4 pb-3 space-y-3">
        {entries.length > 0 ? (
          <div className="flex flex-wrap gap-1.5">
            {entries.map(([key, value]) => (
              <Badge
                key={key}
                variant="secondary"
                className="max-w-full whitespace-normal break-all text-xs font-mono"
              >
                {key}={String(value)}
              </Badge>
            ))}
          </div>
        ) : (
          <p className="text-xs text-muted-foreground">No labels.</p>
        )}
        {showChangeHint && (
          <div className="space-y-1.5">
            <p className="text-xs text-muted-foreground">
              Change labels on the worker itself, as the user the agent runs as.
              An agent of this release reports them within seconds; an older one
              is restarted to pick them up. Check them with{' '}
              <code className="font-mono">temps agent labels list</code>.
            </p>
            <p className="text-xs text-muted-foreground">
              An agent started with <code className="font-mono">--labels</code>{' '}
              or <code className="font-mono">TEMPS_NODE_LABELS</code> keeps
              those labels: the command refuses and says where they are set.
              Remove the override, restart the agent, then run it again.
            </p>
            <div className="flex items-center gap-2 rounded-md border bg-muted/40 px-2 py-1">
              <code className="min-w-0 flex-1 truncate font-mono text-xs">
                {command}
              </code>
              <CopyButton
                minimal
                className="h-6 w-6 shrink-0"
                value={command}
              />
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  )
}
