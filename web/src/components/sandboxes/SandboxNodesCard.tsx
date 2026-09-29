// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ArrowUpRight } from 'lucide-react'
import { toast } from 'sonner'
import {
  getSandboxPlacementOptions,
  getSandboxPlacementQueryKey,
  updateSandboxPlacementMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { PlacementNode } from '@/api/client'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Label } from '@/components/ui/label'
import { useAuth } from '@/contexts/AuthContext-shared'
import { problemDetail } from '@/lib/api-problem'
import { canManageSandboxPlacement } from './helpers'

/**
 * Which nodes may run sandboxes (ADR-048). `null` = every node, the
 * default. The control plane is node `0`. Existing sandboxes are never
 * moved or stopped by this setting — it only governs new placements.
 */
export function SandboxNodesCard() {
  const queryClient = useQueryClient()
  const { user } = useAuth()
  const canEdit = canManageSandboxPlacement(user?.role)
  const placement = useQuery(getSandboxPlacementOptions())
  const [allowAll, setAllowAll] = useState(true)
  const [selected, setSelected] = useState<number[]>([])

  useEffect(() => {
    if (!placement.data) return
    const allowed = placement.data.allowed_node_ids ?? null
    const known = new Set(placement.data.nodes.map((n) => n.id))
    setAllowAll(allowed === null)
    // Only ids that still have a checkbox: a removed node must not linger
    // invisibly in the selection and fail the next save.
    setSelected((allowed ?? [...known]).filter((id) => known.has(id)))
  }, [placement.data])

  const dirty = useMemo(() => {
    if (!placement.data) return false
    const saved = placement.data.allowed_node_ids ?? null
    if (allowAll) return saved !== null
    if (saved === null) return true
    return [...saved].sort().join(',') !== [...selected].sort().join(',')
  }, [placement.data, allowAll, selected])

  const save = useMutation({
    ...updateSandboxPlacementMutation(),
    meta: { errorTitle: 'Failed to update sandbox nodes' },
    onSuccess: (data) => {
      queryClient.setQueryData(getSandboxPlacementQueryKey(), data)
      toast.success('Sandbox nodes updated')
    },
  })

  const toggle = (id: number, checked: boolean) =>
    setSelected((prev) =>
      checked ? [...new Set([...prev, id])] : prev.filter((x) => x !== id)
    )

  const nodes: PlacementNode[] = placement.data?.nodes ?? []
  const onlyControlPlane = nodes.length === 1

  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-base">Sandbox nodes</CardTitle>
        <CardDescription>
          Choose which nodes can run new sandboxes. When a sandbox is created
          without a node, Temps uses the control plane if it is allowed,
          otherwise the allowed worker with the fewest sandboxes. Sandboxes that
          already exist keep running where they are.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {placement.isError && (
          <Alert variant="destructive">
            <AlertDescription className="flex items-center justify-between gap-2">
              <span>
                Could not load sandbox nodes:{' '}
                {problemDetail(placement.error, 'Try again in a moment.')}
              </span>
              <Button
                size="sm"
                variant="outline"
                onClick={() => placement.refetch()}
              >
                Retry
              </Button>
            </AlertDescription>
          </Alert>
        )}
        {placement.isLoading && (
          <p className="text-sm text-muted-foreground">Loading nodes…</p>
        )}
        {placement.data && (
          <>
            <div className="flex items-center gap-2">
              <Checkbox
                id="sandbox-nodes-all"
                checked={allowAll}
                disabled={!canEdit}
                onCheckedChange={(v) => {
                  setAllowAll(v === true)
                  if (v !== true && selected.length === 0) {
                    setSelected(nodes.map((n) => n.id))
                  }
                }}
              />
              <Label htmlFor="sandbox-nodes-all" className="text-sm">
                Allow every node, including nodes added later
              </Label>
            </div>
            <div className="divide-y rounded-md border">
              {nodes.map((node) => {
                const checked = allowAll || selected.includes(node.id)
                return (
                  <div
                    key={node.id}
                    className="flex items-center justify-between gap-3 px-3 py-2"
                  >
                    <div className="flex items-center gap-2 min-w-0">
                      <Checkbox
                        id={`sandbox-node-${node.id}`}
                        checked={checked}
                        disabled={allowAll || !canEdit}
                        onCheckedChange={(v) => toggle(node.id, v === true)}
                      />
                      <Label
                        htmlFor={`sandbox-node-${node.id}`}
                        className="text-sm font-medium truncate"
                      >
                        {node.is_control_plane ? 'Control plane' : node.name}
                      </Label>
                      {!node.is_control_plane && (
                        <Link
                          to={`/settings/nodes/${node.id}`}
                          title={`Open node ${node.name}`}
                          aria-label={`Open node ${node.name}`}
                          className="text-muted-foreground hover:text-foreground"
                        >
                          <ArrowUpRight className="h-3.5 w-3.5" />
                        </Link>
                      )}
                      <Badge
                        variant={
                          node.status === 'active' ? 'outline' : 'secondary'
                        }
                      >
                        {node.status}
                      </Badge>
                    </div>
                    <span className="text-xs text-muted-foreground shrink-0">
                      {node.live_sandboxes} sandbox
                      {node.live_sandboxes === 1 ? '' : 'es'}
                    </span>
                  </div>
                )
              })}
            </div>
            {!allowAll && selected.length === 0 && (
              <p className="text-xs text-destructive">
                With no node selected, nobody can create new sandboxes.
              </p>
            )}
            {onlyControlPlane && (
              <p className="text-xs text-muted-foreground">
                Only the control plane is available. Run{' '}
                <code className="font-mono">temps join</code> on another machine
                to add a worker, then run sandboxes there with{' '}
                <code className="font-mono">
                  bunx @temps-sdk/cli sandbox create --node &lt;worker&gt;
                </code>
                . See{' '}
                <Link
                  to="/settings/nodes"
                  className="underline underline-offset-2"
                >
                  Nodes
                </Link>
                .
              </p>
            )}
            <div className="flex items-center justify-end gap-3">
              {!canEdit && (
                <span className="text-xs text-muted-foreground">
                  Only administrators can change which nodes run sandboxes.
                </span>
              )}
              <Button
                size="sm"
                disabled={!canEdit || !dirty || save.isPending}
                onClick={() =>
                  save.mutate({
                    body: { allowed_node_ids: allowAll ? null : selected },
                  })
                }
              >
                {save.isPending ? 'Saving…' : 'Save'}
              </Button>
            </div>
          </>
        )}
      </CardContent>
    </Card>
  )
}
