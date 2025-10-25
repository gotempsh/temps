'use client'

import { useState } from 'react'
import { ConnectionResponse, ProviderResponse } from '@/api/client/types.gen'
import { deleteConnectionMutation } from '@/api/client/@tanstack/react-query.gen'
import { UpdateTokenDialog } from '@/components/git/UpdateTokenDialog'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
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
import { TimeAgo } from '@/components/utils/TimeAgo'
import {
  CheckCircle2,
  XCircle,
  RefreshCw,
  Users,
  Clock,
  ChevronLeft,
  ChevronRight,
  ChevronsLeft,
  ChevronsRight,
  ExternalLink,
  Key,
  Trash2,
} from 'lucide-react'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'

interface ConnectionsTableProps {
  connections: ConnectionResponse[]
  provider?: ProviderResponse
  onSyncRepository: (connectionId: number) => void
  onAuthorize?: () => void
  isSyncing: boolean
  isAuthorizing?: boolean
  onConnectionDeleted?: () => void
}

const PAGE_SIZES = [5, 10, 20, 50] as const

export function ConnectionsTable({
  connections,
  provider,
  onSyncRepository,
  onAuthorize,
  isSyncing,
  isAuthorizing = false,
  onConnectionDeleted,
}: ConnectionsTableProps) {
  const queryClient = useQueryClient()
  const [currentPage, setCurrentPage] = useState(1)
  const [pageSize, setPageSize] = useState(10)
  const [updateTokenDialog, setUpdateTokenDialog] = useState<{
    open: boolean
    connectionId: number
    connectionName: string
  }>({
    open: false,
    connectionId: 0,
    connectionName: '',
  })
  const [deleteDialog, setDeleteDialog] = useState<{
    open: boolean
    connectionId: number
    connectionName: string
  }>({
    open: false,
    connectionId: 0,
    connectionName: '',
  })

  const deleteConnectionMut = useMutation({
    ...deleteConnectionMutation(),
    onSuccess: () => {
      toast.success('Connection deleted successfully')
      queryClient.invalidateQueries({ queryKey: ['listConnections'] })
      setDeleteDialog({ open: false, connectionId: 0, connectionName: '' })
      // Call the callback to refresh connections in parent component
      onConnectionDeleted?.()
    },
    onError: () => {
      toast.error('Failed to delete connection')
    },
  })

  const totalItems = connections.length
  const totalPages = Math.ceil(totalItems / pageSize)
  const startIndex = (currentPage - 1) * pageSize
  const endIndex = Math.min(startIndex + pageSize, totalItems)
  const currentConnections = connections.slice(startIndex, endIndex)

  const goToPage = (page: number) => {
    setCurrentPage(Math.max(1, Math.min(page, totalPages)))
  }

  const goToFirstPage = () => goToPage(1)
  const goToLastPage = () => goToPage(totalPages)
  const goToPreviousPage = () => goToPage(currentPage - 1)
  const goToNextPage = () => goToPage(currentPage + 1)

  const handlePageSizeChange = (newPageSize: string) => {
    const size = parseInt(newPageSize)
    setPageSize(size)
    setCurrentPage(1) // Reset to first page when changing page size
  }

  // Helper to check if provider is OAuth-based (GitHub App or GitLab OAuth)
  const isOAuthProvider =
    provider &&
    ((provider.provider_type === 'github' &&
      (provider.auth_method === 'app' ||
        provider.auth_method === 'github_app')) ||
      (provider.provider_type === 'gitlab' && provider.auth_method === 'oauth'))

  // Helper to check if provider is PAT-based (GitHub PAT or GitLab PAT)
  const isPATProvider =
    provider &&
    ((provider.provider_type === 'github' &&
      (provider.auth_method === 'pat' ||
        provider.auth_method === 'github_pat')) ||
      (provider.provider_type === 'gitlab' &&
        (provider.auth_method === 'pat' ||
          provider.auth_method === 'gitlab_pat')))

  if (totalItems === 0) {
    return null // Let parent handle empty state
  }

  return (
    <div className="space-y-4">
      {/* Action Buttons */}
      {isOAuthProvider && onAuthorize && (
        <div className="flex justify-end">
          <Button
            variant="default"
            size="sm"
            onClick={onAuthorize}
            disabled={isAuthorizing}
            className="gap-2"
          >
            <ExternalLink className="h-4 w-4" />
            {isAuthorizing ? 'Authorizing...' : 'Authorize'}
          </Button>
        </div>
      )}

      {/* Table */}
      <div className="overflow-x-auto">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Account</TableHead>
              <TableHead>Type</TableHead>
              <TableHead>Status</TableHead>
              <TableHead>Installation ID</TableHead>
              <TableHead>Last Synced</TableHead>
              <TableHead>Created</TableHead>
              <TableHead>Actions</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {currentConnections.map((connection: ConnectionResponse) => (
              <TableRow key={connection.id}>
                <TableCell>
                  <div className="flex items-center gap-2">
                    <Users className="h-4 w-4" />
                    <span className="font-medium">
                      {connection.account_name}
                    </span>
                  </div>
                </TableCell>
                <TableCell>
                  <Badge variant="outline">
                    {connection.account_type?.charAt(0).toUpperCase() +
                      connection.account_type?.slice(1) || 'Unknown'}
                  </Badge>
                </TableCell>
                <TableCell>
                  <div className="flex items-center gap-2">
                    {connection.is_active ? (
                      <Badge
                        variant="secondary"
                        className="flex items-center gap-1"
                      >
                        <CheckCircle2 className="h-3 w-3" />
                        Active
                      </Badge>
                    ) : (
                      <Badge
                        variant="destructive"
                        className="flex items-center gap-1"
                      >
                        <XCircle className="h-3 w-3" />
                        Inactive
                      </Badge>
                    )}
                    {connection.syncing && (
                      <Badge
                        variant="outline"
                        className="flex items-center gap-1"
                      >
                        <RefreshCw className="h-3 w-3 animate-spin" />
                        Syncing
                      </Badge>
                    )}
                  </div>
                </TableCell>
                <TableCell>
                  {connection.installation_id ? (
                    <span className="font-mono text-sm">
                      {connection.installation_id}
                    </span>
                  ) : (
                    <span className="text-muted-foreground">-</span>
                  )}
                </TableCell>
                <TableCell>
                  <div className="flex items-center gap-1">
                    <Clock className="h-3 w-3 text-muted-foreground" />
                    {connection.last_synced_at ? (
                      <TimeAgo
                        date={connection.last_synced_at}
                        className="text-sm"
                      />
                    ) : (
                      <span className="text-sm text-muted-foreground">
                        Never
                      </span>
                    )}
                  </div>
                </TableCell>
                <TableCell>
                  <TimeAgo
                    date={connection.created_at}
                    className="text-sm text-muted-foreground"
                  />
                </TableCell>
                <TableCell>
                  <div className="flex items-center gap-1">
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => onSyncRepository(connection.id)}
                      disabled={isSyncing}
                      title="Sync repositories"
                    >
                      <RefreshCw
                        className={`h-4 w-4 ${isSyncing ? 'animate-spin' : ''}`}
                      />
                    </Button>
                    {/* Only show Update Token button for PAT-based providers */}
                    {isPATProvider && (
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() =>
                          setUpdateTokenDialog({
                            open: true,
                            connectionId: connection.id,
                            connectionName: connection.account_name,
                          })
                        }
                        title="Update access token"
                      >
                        <Key className="h-4 w-4" />
                      </Button>
                    )}
                    {/* Only show Delete button for OAuth-based providers */}
                    {isOAuthProvider && (
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() =>
                          setDeleteDialog({
                            open: true,
                            connectionId: connection.id,
                            connectionName: connection.account_name,
                          })
                        }
                        title="Delete connection"
                        className="text-destructive hover:text-destructive"
                      >
                        <Trash2 className="h-4 w-4" />
                      </Button>
                    )}
                  </div>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </div>

      {/* Pagination Controls */}
      {totalPages > 1 && (
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <span className="text-sm text-muted-foreground">
              Showing {startIndex + 1} to {endIndex} of {totalItems} connections
            </span>
          </div>

          <div className="flex items-center gap-2">
            {/* Page size selector */}
            <div className="flex items-center gap-2">
              <span className="text-sm text-muted-foreground">Show</span>
              <Select
                value={pageSize.toString()}
                onValueChange={handlePageSizeChange}
              >
                <SelectTrigger className="w-20">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {PAGE_SIZES.map((size) => (
                    <SelectItem key={size} value={size.toString()}>
                      {size}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>

            {/* Pagination buttons */}
            <div className="flex items-center gap-1">
              <Button
                variant="outline"
                size="sm"
                onClick={goToFirstPage}
                disabled={currentPage === 1}
                title="First page"
              >
                <ChevronsLeft className="h-4 w-4" />
              </Button>
              <Button
                variant="outline"
                size="sm"
                onClick={goToPreviousPage}
                disabled={currentPage === 1}
                title="Previous page"
              >
                <ChevronLeft className="h-4 w-4" />
              </Button>

              <span className="px-2 text-sm">
                Page {currentPage} of {totalPages}
              </span>

              <Button
                variant="outline"
                size="sm"
                onClick={goToNextPage}
                disabled={currentPage === totalPages}
                title="Next page"
              >
                <ChevronRight className="h-4 w-4" />
              </Button>
              <Button
                variant="outline"
                size="sm"
                onClick={goToLastPage}
                disabled={currentPage === totalPages}
                title="Last page"
              >
                <ChevronsRight className="h-4 w-4" />
              </Button>
            </div>
          </div>
        </div>
      )}

      {/* Update Token Dialog */}
      <UpdateTokenDialog
        connectionId={updateTokenDialog.connectionId}
        connectionName={updateTokenDialog.connectionName}
        open={updateTokenDialog.open}
        onOpenChange={(open) =>
          setUpdateTokenDialog({
            ...updateTokenDialog,
            open,
          })
        }
      />

      {/* Delete Connection Confirmation Dialog */}
      <AlertDialog
        open={deleteDialog.open}
        onOpenChange={(open) =>
          setDeleteDialog({
            ...deleteDialog,
            open,
          })
        }
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete Connection</AlertDialogTitle>
            <AlertDialogDescription>
              Are you sure you want to delete the connection for{' '}
              <strong>{deleteDialog.connectionName}</strong>? This action cannot
              be undone and will remove all associated repositories from this
              connection.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={deleteConnectionMut.isPending}>
              Cancel
            </AlertDialogCancel>
            <AlertDialogAction
              onClick={() =>
                deleteConnectionMut.mutate({
                  path: { connection_id: deleteDialog.connectionId },
                })
              }
              disabled={deleteConnectionMut.isPending}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              {deleteConnectionMut.isPending ? (
                <>
                  <RefreshCw className="mr-2 h-4 w-4 animate-spin" />
                  Deleting...
                </>
              ) : (
                <>
                  <Trash2 className="mr-2 h-4 w-4" />
                  Delete
                </>
              )}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}
