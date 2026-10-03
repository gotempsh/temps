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
  getSandboxPlacementQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { evictNodeSandboxes } from '@/api/client/sdk.gen'
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
import { CopyButton } from '@/components/ui/copy-button'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useAuth } from '@/contexts/AuthContext-shared'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { problemDetail } from '@/lib/api-problem'
import {
  evictionReportFromProblem,
  evictionReportFromResponse,
  evictionReportHasDetails,
  evictionReportNeedsAttention,
  isEvictionInProgress,
  withHttpStatus,
  type EvictionReport,
} from './node-eviction'

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
  // What the last eviction left behind (unconfirmed containers, sandboxes it
  // could not destroy). Kept on screen, not only in a toast, because the
  // operator has to copy the cleanup commands onto the node.
  const [report, setReport] = useState<EvictionReport | null>(null)
  // Evicting destroys other users' data: browser sessions re-verify (MFA),
  // the same as draining a node.
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()
  const evict = useMutation({
    // The generated mutation throws only the Problem body, which carries no
    // HTTP status; keep it so a 409 and a 503 can be told apart.
    mutationFn: async (variables: { path: { node: string } }) => {
      const { data, error, response } = await evictNodeSandboxes({
        ...variables,
        throwOnError: false,
      })
      if (error !== undefined || data === undefined) {
        throw withHttpStatus(error, response?.status)
      }
      return data
    },
    onSuccess: (data) => {
      const result = evictionReportFromResponse(data)
      setReport(evictionReportNeedsAttention(result) ? result : null)
      if (result.containersUnconfirmed.length > 0) {
        toast.warning(
          `Destroyed ${result.destroyed.length} sandbox(es) on ${data.node.name}, but the node did not confirm removing ${result.containersUnconfirmed.length} container(s)`,
          {
            description:
              'They may still be running on the node. The Sandboxes tab lists them with the commands that remove them.',
            duration: 15_000,
          }
        )
      } else {
        toast.success(
          `Destroyed ${result.destroyed.length} sandbox(es) on ${data.node.name}`
        )
      }
      onPageChange(1)
    },
    onError: (error, variables) => {
      if (handleSensitiveActionError(error, () => evict.mutate(variables)))
        return
      if (isEvictionInProgress(error)) {
        toast.info(`Sandboxes on ${nodeName} are already being destroyed`, {
          description: problemDetail(
            error,
            'Another eviction of this node is still running. This list refreshes as it progresses.'
          ),
        })
        return
      }
      const partial = evictionReportFromProblem(error)
      if (partial) {
        setReport(partial)
        toast.error(
          partial.failed.length > 0
            ? `Could not destroy ${partial.failed.length} sandbox(es) on ${nodeName}`
            : `Some sandboxes on ${nodeName} could not be destroyed`,
          {
            description:
              'The Sandboxes tab lists what was destroyed and what was left. Run Destroy all again to retry.',
            duration: 15_000,
          }
        )
        return
      }
      toast.error('Could not destroy the sandboxes on this node', {
        description: problemDetail(error, 'Try again in a moment.'),
      })
    },
    onSettled: () => {
      setConfirmEvict(false)
      // A partial eviction still destroyed some sandboxes. The node page's
      // Remove button reads the drain status, and the placement card shows
      // per-node counts, so refresh those too.
      const path = { path: { node_id: nodeId } }
      void queryClient.invalidateQueries({
        queryKey: [{ _id: 'listNodeSandboxes' }],
      })
      void queryClient.invalidateQueries({
        queryKey: getSandboxPlacementQueryKey(),
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
    return <NodeSandboxesSkeleton />
  }

  // A failed background refresh keeps showing the last good list (and any
  // open dialog); only a list that never loaded is replaced by the error.
  if (!query.data) {
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
        {query.isError && (
          <div className="flex items-center justify-between gap-2 border-b px-4 py-2 text-xs text-destructive">
            <span>
              Could not refresh this list:{' '}
              {problemDetail(query.error, 'Try again in a moment.')}
            </span>
            <Button size="sm" variant="outline" onClick={() => query.refetch()}>
              Retry
            </Button>
          </div>
        )}
        {report && (
          <EvictionReportAlert
            report={report}
            onDismiss={() => setReport(null)}
          />
        )}
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
                        {/* Other users' sandboxes have no detail page, so
                            the columns hidden on small screens stay
                            readable here as secondary text. */}
                        <div className="mt-0.5 space-y-0.5 text-xs text-muted-foreground lg:hidden">
                          <p className="md:hidden">
                            {kindLabel(sandbox.lifecycle)} · created{' '}
                            {createdAgo(sandbox.createdAt)}
                          </p>
                          <p className="max-w-[220px] truncate font-mono">
                            {sandbox.image ?? 'platform default'}
                          </p>
                        </div>
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
                        {kindLabel(sandbox.lifecycle)}
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
                        {createdAgo(sandbox.createdAt)}
                      </TableCell>
                    </TableRow>
                  )
                })}
              </TableBody>
            </Table>
            {pages > 1 && (
              <div className="border-t px-4 py-2">
                <ResponsivePagination
                  page={page}
                  pageSize={pageSize}
                  total={total}
                  totalPages={pages}
                  onPageChange={onPageChange}
                  ariaLabel="Sandboxes on this node"
                />
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
      {verificationDialog}
    </Card>
  )
}

function kindLabel(lifecycle: string | undefined): string {
  return lifecycle === 'workspace' ? 'Workspace' : 'Ephemeral'
}

function createdAgo(createdAt: number): string {
  return formatDistanceToNow(new Date(createdAt), { addSuffix: true })
}

/** Placeholder rows matching the loaded table, so the tab does not jump. */
export function NodeSandboxesSkeleton() {
  return (
    <Card aria-busy="true" aria-label="Loading sandboxes on this node">
      <CardContent className="px-0 pb-0 pt-0">
        <div className="flex flex-wrap items-center justify-between gap-2 border-b px-4 py-3">
          <Skeleton className="h-4 w-56" />
          <Skeleton className="h-8 w-40" />
        </div>
        <div className="overflow-x-auto">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Sandbox</TableHead>
                <TableHead>Status</TableHead>
                <TableHead className="hidden md:table-cell">Kind</TableHead>
                <TableHead>Owner</TableHead>
                <TableHead className="hidden lg:table-cell">Image</TableHead>
                <TableHead className="hidden md:table-cell">Created</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {[0, 1, 2].map((row) => (
                <TableRow key={row} data-testid="node-sandbox-skeleton-row">
                  <TableCell>
                    <Skeleton className="h-4 w-28" />
                  </TableCell>
                  <TableCell>
                    <Skeleton className="h-5 w-16" />
                  </TableCell>
                  <TableCell className="hidden md:table-cell">
                    <Skeleton className="h-4 w-20" />
                  </TableCell>
                  <TableCell>
                    <Skeleton className="h-4 w-32" />
                  </TableCell>
                  <TableCell className="hidden lg:table-cell">
                    <Skeleton className="h-4 w-40" />
                  </TableCell>
                  <TableCell className="hidden md:table-cell">
                    <Skeleton className="h-4 w-24" />
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      </CardContent>
    </Card>
  )
}

/**
 * What the last eviction left behind: containers the node did not confirm
 * removing (with the command that removes each one) and sandboxes it could
 * not destroy. Rendered the same way for a full and a partial eviction.
 */
export function EvictionReportAlert({
  report,
  onDismiss,
}: {
  report: EvictionReport
  onDismiss: () => void
}) {
  const { destroyed, containersUnconfirmed, failed } = report
  return (
    <Alert
      variant={report.partial ? 'destructive' : 'default'}
      className="rounded-none border-x-0 border-t-0"
    >
      <AlertDescription className="space-y-3 text-sm">
        {report.partial && (
          <p>
            {destroyed.length > 0
              ? `Destroyed ${destroyed.length} sandbox(es), but not all of them.`
              : 'Not every sandbox on this node could be destroyed.'}{' '}
            Run Destroy all again to retry the rest.
          </p>
        )}
        {report.partial &&
          !evictionReportHasDetails(report) &&
          report.detail && (
            <p className="whitespace-pre-wrap break-words">{report.detail}</p>
          )}
        {failed.length > 0 && (
          <div className="space-y-1">
            <p className="font-medium">
              Could not destroy {failed.length} sandbox(es):
            </p>
            <ul className="space-y-0.5">
              {failed.map((f) => (
                <li key={f.sandbox_id} className="text-xs">
                  <span className="font-mono">{f.sandbox_id}</span>
                  {f.reason ? `: ${f.reason}` : ''}
                </li>
              ))}
            </ul>
          </div>
        )}
        {containersUnconfirmed.length > 0 && (
          <div className="space-y-2">
            <p>
              The node did not confirm removing the containers of{' '}
              {containersUnconfirmed.length} destroyed sandbox(es). They may
              still be running there, and nothing in Temps tracks them any
              more. If the node comes back, run these on it to remove them. If
              it is gone for good, remove the node.
            </p>
            <ul className="space-y-1">
              {containersUnconfirmed.map((c) => (
                <li key={c.sandbox_id} className="space-y-0.5">
                  <div className="flex items-center gap-2">
                    <code className="min-w-0 flex-1 truncate rounded bg-muted px-2 py-1 font-mono text-xs text-foreground">
                      {c.cleanup_command}
                    </code>
                    <CopyButton
                      value={c.cleanup_command}
                      label={`Copy the command that removes ${c.sandbox_id}`}
                    />
                  </div>
                  <p className="text-xs text-muted-foreground">
                    {c.sandbox_id}
                    {c.reason ? `: ${c.reason}` : ''}
                  </p>
                </li>
              ))}
            </ul>
          </div>
        )}
        {report.partial && destroyed.length > 0 && (
          <p className="text-xs text-muted-foreground">
            Destroyed:{' '}
            <span className="font-mono">{destroyed.join(', ')}</span>
          </p>
        )}
        <Button size="sm" variant="outline" onClick={onDismiss}>
          Dismiss
        </Button>
      </AlertDescription>
    </Alert>
  )
}
