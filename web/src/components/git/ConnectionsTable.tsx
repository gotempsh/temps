'use client'

import { useState } from 'react'
import { ConnectionResponse, ProviderResponse } from '@/api/client/types.gen'
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
} from 'lucide-react'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'

interface ConnectionsTableProps {
  connections: ConnectionResponse[]
  provider?: ProviderResponse
  onSyncRepository: (connectionId: number) => void
  onAuthorize?: () => void
  isSyncing: boolean
  isAuthorizing?: boolean
}

const PAGE_SIZES = [5, 10, 20, 50] as const

export function ConnectionsTable({
  connections,
  provider,
  onSyncRepository,
  onAuthorize,
  isSyncing,
  isAuthorizing = false,
}: ConnectionsTableProps) {
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
    </div>
  )
}
