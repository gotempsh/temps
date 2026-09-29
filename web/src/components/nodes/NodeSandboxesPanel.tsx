// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from 'react'
import { Link } from 'react-router'
import { formatDistanceToNow } from 'date-fns'
import { Loader2, Terminal, Trash2 } from 'lucide-react'
import {
  useMutation,
  useQueryClient,
  type UseQueryResult,
} from '@tanstack/react-query'
import { toast } from 'sonner'
import {
  adminDrainStatusQueryKey,
  adminGetNodeQueryKey,
  evictNodeSandboxesMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { NodeSandboxesResponse } from '@/api/client'
import { Alert, AlertDescription } from '@/components/ui/alert'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useAuth } from '@/contexts/AuthContext-shared'
import { problemDetail } from '@/lib/api-problem'

const SANDBOX_SETTINGS_URL = '/agent-sandbox/sandbox'

/**
 * Every live sandbox on a worker node, from all users (ADR-048). The query
 * lives in the page so the tab strip can show the count before this panel
 * mounts.
 */
export function NodeSandboxesPanel({
  nodeId,
  nodeName,
  canSee,
  query,
  page,
  onPageChange,
}: {
  /** Addressed by id: the server reads a digit-only name as an id. */
  nodeId: number
  nodeName: string
  /** Listing other users' sandboxes is admin-only on the server. */
  canSee: boolean
  query: UseQueryResult<NodeSandboxesResponse, Error>
  page: number
  onPageChange: (page: number) => void
}) {
  const { user } = useAuth()
  const queryClient = useQueryClient()
  const [confirmEvict, setConfirmEvict] = useState(false)
  const evict = useMutation({
    ...evictNodeSandboxesMutation(),
    onSuccess: (data) => {
      toast.success(
        `Destroyed ${data.destroyed.length} sandbox(es) on ${data.node.name}`
      )
      onPageChange(1)
    },
    onError: (error) => {
      toast.error('Could not destroy the sandboxes on this node', {
        description: problemDetail(error, 'Try again in a moment.'),
      })
    },
    onSettled: () => {
      setConfirmEvict(false)
      // A partial eviction still destroyed some sandboxes. The node page's
      // Remove button reads the drain status, so refresh it too.
      const path = { path: { node_id: nodeId } }
      void queryClient.invalidateQueries({
        queryKey: [{ _id: 'listNodeSandboxes' }],
      })
      void queryClient.invalidateQueries({
        queryKey: adminDrainStatusQueryKey(path),
      })
      void queryClient.invalidateQueries({
        queryKey: adminGetNodeQueryKey(path),
      })
    },
  })

  // The list can shrink under us (polling, another admin evicting); never
  // strand the user on a page past the end.
  const total = query.data?.total ?? 0
  const pageSize = query.data?.page_size ?? 20
  const pages = Math.max(1, Math.ceil(total / pageSize))
  useEffect(() => {
    if (query.data && page > pages) onPageChange(pages)
  }, [query.data, page, pages, onPageChange])

  if (!canSee) {
    return (
      <Card>
        <CardContent className="py-8 text-center text-sm text-muted-foreground">
          Only administrators can see the sandboxes on a node, because they
          belong to every user.
        </CardContent>
      </Card>
    )
  }

  if (query.isLoading) {
    return (
      <Card>
        <CardContent className="flex items-center justify-center py-8">
          <Loader2 className="h-5 w-5 animate-spin" />
        </CardContent>
      </Card>
    )
  }

  if (query.isError || !query.data) {
    return (
      <Alert variant="destructive">
        <AlertDescription className="flex items-center justify-between gap-2">
          <span>
            Could not load sandboxes on this node:{' '}
            {problemDetail(query.error, 'Try again in a moment.')}
          </span>
          <Button size="sm" variant="outline" onClick={() => query.refetch()}>
            Retry
          </Button>
        </AlertDescription>
      </Alert>
    )
  }

  const { node, sandboxes } = query.data

  return (
    <Card>
      <CardContent className="px-0 pb-0 pt-0">
        <div className="flex flex-wrap items-center justify-between gap-2 border-b px-4 py-3 text-sm">
          <span className="text-muted-foreground">
            {node.eligible
              ? 'This node accepts new sandboxes.'
              : `This node does not accept new sandboxes: ${node.reason ?? 'not eligible'}. Existing sandboxes keep running.`}
          </span>
          <div className="flex items-center gap-3">
            <Link
              to={SANDBOX_SETTINGS_URL}
              className="text-sm underline underline-offset-2"
            >
              Sandbox placement settings
            </Link>
            {total > 0 && (
              <Button
                size="sm"
                variant="outline"
                className="text-destructive"
                onClick={() => setConfirmEvict(true)}
              >
                <Trash2 className="mr-1 h-4 w-4" />
                Destroy all
              </Button>
            )}
          </div>
        </div>

        {sandboxes.length === 0 ? (
          <div className="flex flex-col items-center justify-center px-4 py-8 text-center">
            <Terminal className="mb-2 h-8 w-8 text-muted-foreground" />
            <p className="text-sm text-muted-foreground">
              No sandboxes on this node.
            </p>
            {node.eligible && (
              <p className="mt-1 text-xs text-muted-foreground">
                Run one here with{' '}
                <code className="font-mono">
                  bunx @temps-sdk/cli sandbox create --node {nodeName}
                </code>
              </p>
            )}
          </div>
        ) : (
          <div className="overflow-x-auto">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Sandbox</TableHead>
                  <TableHead>Status</TableHead>
                  <TableHead className="hidden md:table-cell">Kind</TableHead>
                  <TableHead>Owner</TableHead>
                  <TableHead className="hidden lg:table-cell">Image</TableHead>
                  <TableHead className="hidden md:table-cell">
                    Created
                  </TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {sandboxes.map(({ sandbox, owner_user_id, owner_email }) => {
                  // The sandbox page is owner-scoped; link only to our own.
                  const ownedByMe = user != null && owner_user_id === user.id
                  return (
                    <TableRow key={sandbox.id}>
                      <TableCell>
                        {ownedByMe ? (
                          <Link
                            to={`/sandboxes/${sandbox.id}`}
                            className="font-mono text-xs underline underline-offset-2"
                          >
                            {sandbox.id}
                          </Link>
                        ) : (
                          <span className="font-mono text-xs">
                            {sandbox.id}
                          </span>
                        )}
                      </TableCell>
                      <TableCell>
                        <Badge
                          variant={
                            sandbox.status === 'running'
                              ? 'default'
                              : 'secondary'
                          }
                          className={`text-xs ${
                            sandbox.status === 'running'
                              ? 'bg-green-500/15 text-green-700 dark:text-green-400 border-green-500/20'
                              : ''
                          }`}
                        >
                          {sandbox.status}
                        </Badge>
                      </TableCell>
                      <TableCell className="hidden md:table-cell text-sm">
                        {sandbox.lifecycle === 'workspace'
                          ? 'Workspace'
                          : 'Ephemeral'}
                      </TableCell>
                      <TableCell className="text-sm">
                        {owner_email ??
                          (owner_user_id != null
                            ? `User ${owner_user_id}`
                            : 'System')}
                      </TableCell>
                      <TableCell className="hidden lg:table-cell">
                        <span className="block max-w-[250px] truncate font-mono text-xs text-muted-foreground">
                          {sandbox.image ?? 'platform default'}
                        </span>
                      </TableCell>
                      <TableCell className="hidden md:table-cell text-sm text-muted-foreground">
                        {formatDistanceToNow(new Date(sandbox.createdAt), {
                          addSuffix: true,
                        })}
                      </TableCell>
                    </TableRow>
                  )
                })}
              </TableBody>
            </Table>
            {pages > 1 && (
              <div className="flex items-center justify-between border-t px-4 py-2 text-xs text-muted-foreground">
                <span>
                  Page {page} of {pages} · {total} sandboxes
                </span>
                <div className="flex gap-2">
                  <Button
                    size="sm"
                    variant="outline"
                    disabled={page <= 1}
                    onClick={() => onPageChange(page - 1)}
                  >
                    Previous
                  </Button>
                  <Button
                    size="sm"
                    variant="outline"
                    disabled={page >= pages}
                    onClick={() => onPageChange(page + 1)}
                  >
                    Next
                  </Button>
                </div>
              </div>
            )}
          </div>
        )}
      </CardContent>

      <AlertDialog open={confirmEvict} onOpenChange={setConfirmEvict}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              Destroy all {total} sandbox(es) on {nodeName}?
            </AlertDialogTitle>
            <AlertDialogDescription>
              This destroys every sandbox on this node, including other
              users&rsquo; sandboxes and their files. It works even if the node
              is offline for good, so you can remove the node afterwards. This
              cannot be undone.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={evict.isPending}>
              Cancel
            </AlertDialogCancel>
            <AlertDialogAction
              disabled={evict.isPending}
              className="bg-destructive text-white hover:bg-destructive/90"
              onClick={(e) => {
                e.preventDefault()
                evict.mutate({ path: { node: String(nodeId) } })
              }}
            >
              {evict.isPending && (
                <Loader2 className="mr-1 h-4 w-4 animate-spin" />
              )}
              Destroy all
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </Card>
  )
}
