import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import {
  MoreHorizontal,
  Key,
  Shield,
  ChevronDown,
  ChevronUp,
} from 'lucide-react'
import { format } from 'date-fns'
import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { getApiKeyPermissionsOptions } from '@/api/client/@tanstack/react-query.gen'
import type { ApiKeyResponse } from '@/api/client'

interface ApiKeyTableProps {
  apiKeys: ApiKeyResponse[] | undefined
  isLoading: boolean
  onView: (key: ApiKeyResponse) => void
  onEdit: (key: ApiKeyResponse) => void
  onDelete: (key: ApiKeyResponse) => void
  onActivate: (id: number) => void
  onDeactivate: (id: number) => void
  onCreateClick: () => void
}

// Component for showing permissions with expandable list
function PermissionsDisplay({ apiKey }: { apiKey: ApiKeyResponse }) {
  const [expanded, setExpanded] = useState(false)

  const { data: permissionsData } = useQuery({
    ...getApiKeyPermissionsOptions({}),
  })

  // Get role-based permissions if not custom
  const rolePermissions =
    apiKey.role_type !== 'custom'
      ? permissionsData?.roles.find((r) => r.name === apiKey.role_type)
          ?.permissions || []
      : []

  // Use custom permissions or role permissions
  const allPermissions =
    apiKey.role_type === 'custom' && apiKey.permissions
      ? apiKey.permissions
      : rolePermissions

  if (!allPermissions || allPermissions.length === 0) {
    return <Badge variant="outline">{apiKey.role_type}</Badge>
  }

  const displayPermissions = expanded
    ? allPermissions
    : allPermissions.slice(0, 10)
  const hasMore = allPermissions.length > 10

  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <div className="flex items-center gap-1">
          <Shield className="h-3 w-3" />
          <Badge variant="outline" className="cursor-help">
            {apiKey.role_type === 'custom' ? 'Custom' : apiKey.role_type} (
            {allPermissions.length})
          </Badge>
        </div>
      </TooltipTrigger>
      <TooltipContent className="max-w-md" side="left">
        <div className="space-y-2">
          <div className="flex items-center justify-between">
            <p className="font-semibold text-xs">
              {apiKey.role_type === 'custom'
                ? 'Custom Permissions:'
                : `${apiKey.role_type} Role Permissions:`}
            </p>
            {hasMore && (
              <Button
                variant="ghost"
                size="sm"
                className="h-4 px-1"
                onClick={() => setExpanded(!expanded)}
              >
                {expanded ? (
                  <ChevronUp className="h-3 w-3" />
                ) : (
                  <ChevronDown className="h-3 w-3" />
                )}
              </Button>
            )}
          </div>
          <div className="text-xs space-y-0.5 max-h-60 overflow-y-auto">
            {displayPermissions.map((p) => (
              <div key={p}>• {p}</div>
            ))}
            {!expanded && hasMore && (
              <Button
                variant="ghost"
                size="sm"
                className="text-muted-foreground text-xs h-auto p-0 mt-1"
                onClick={() => setExpanded(true)}
              >
                ...and {allPermissions.length - 10} more
              </Button>
            )}
          </div>
        </div>
      </TooltipContent>
    </Tooltip>
  )
}

export function ApiKeyTable({
  apiKeys,
  isLoading,
  onView,
  onEdit,
  onDelete,
  onActivate,
  onDeactivate,
  onCreateClick,
}: ApiKeyTableProps) {
  if (isLoading) {
    return <div className="text-center py-8">Loading...</div>
  }

  if (!apiKeys || apiKeys.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center py-12 space-y-4">
        <div className="rounded-full bg-muted p-4">
          <Key className="h-8 w-8 text-muted-foreground" />
        </div>
        <div className="text-center space-y-2">
          <h3 className="text-lg font-semibold">No API keys yet</h3>
          <p className="text-sm text-muted-foreground max-w-sm">
            Create your first API key to enable programmatic access to your
            resources
          </p>
        </div>
        <Button onClick={onCreateClick}>Create Your First API Key</Button>
      </div>
    )
  }

  return (
    <TooltipProvider>
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Name</TableHead>
            <TableHead>Key Prefix</TableHead>
            <TableHead>Access Level</TableHead>
            <TableHead>Status</TableHead>
            <TableHead>Created</TableHead>
            <TableHead>Last Used</TableHead>
            <TableHead>Expires</TableHead>
            <TableHead className="text-right">Actions</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {apiKeys.map((key) => (
            <TableRow key={key.id}>
              <TableCell className="font-medium">
                <button
                  onClick={() => onView(key)}
                  className="hover:underline cursor-pointer text-left"
                >
                  {key.name}
                </button>
              </TableCell>
              <TableCell>
                <code className="text-xs bg-muted px-1 py-0.5 rounded">
                  {key.key_prefix}...
                </code>
              </TableCell>
              <TableCell>
                <PermissionsDisplay apiKey={key} />
              </TableCell>
              <TableCell>
                <Badge variant={key.is_active ? 'default' : 'secondary'}>
                  {key.is_active ? 'Active' : 'Inactive'}
                </Badge>
              </TableCell>
              <TableCell>
                {format(new Date(key.created_at), 'MMM d, yyyy')}
              </TableCell>
              <TableCell>
                {key.last_used_at
                  ? format(new Date(key.last_used_at), 'MMM d, yyyy HH:mm')
                  : 'Never'}
              </TableCell>
              <TableCell>
                {key.expires_at ? (
                  <span
                    className={
                      new Date(key.expires_at) < new Date()
                        ? 'text-destructive'
                        : ''
                    }
                  >
                    {format(new Date(key.expires_at), 'MMM d, yyyy')}
                  </span>
                ) : (
                  'Never'
                )}
              </TableCell>
              <TableCell className="text-right">
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <Button variant="ghost" size="sm">
                      <MoreHorizontal className="h-4 w-4" />
                    </Button>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end">
                    <DropdownMenuItem onClick={() => onView(key)}>
                      View Details
                    </DropdownMenuItem>
                    <DropdownMenuItem onClick={() => onEdit(key)}>
                      Edit
                    </DropdownMenuItem>
                    {key.is_active ? (
                      <DropdownMenuItem onClick={() => onDeactivate(key.id)}>
                        Deactivate
                      </DropdownMenuItem>
                    ) : (
                      <DropdownMenuItem onClick={() => onActivate(key.id)}>
                        Activate
                      </DropdownMenuItem>
                    )}
                    <DropdownMenuItem
                      className="text-destructive"
                      onClick={() => onDelete(key)}
                    >
                      Delete
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </TooltipProvider>
  )
}
