// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from 'react'
import { useNavigate, useParams } from 'react-router'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from '@/components/ui/alert-dialog'
import {
  getApiKey,
  deleteApiKey,
  activateApiKey,
  deactivateApiKey,
} from '@/api/client'
import { useApiKeyPermissions } from '@/components/api-keys/useApiKeyPermissions'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  Button,
  Callout,
  Detail,
  PageState,
  Status,
  fmtDateTime,
  fmtRelativeTime,
  type DetailFact,
  type StatusTone,
} from '@temps-sdk/ds'
import {
  AlertCircle,
  ArrowLeft,
  Check,
  Key,
  Shield,
  Trash2,
  X,
} from 'lucide-react'
import { toast } from 'sonner'

// Helper component to display permissions with show more/less functionality
interface PermissionsDisplayProps {
  permissions: string[]
}

function PermissionsDisplay({ permissions }: PermissionsDisplayProps) {
  const [showAll, setShowAll] = useState(false)
  const displayPermissions = showAll ? permissions : permissions.slice(0, 10)
  const hasMore = permissions.length > 10

  return (
    <>
      <div className="mt-2 flex flex-wrap gap-2">
        {displayPermissions.map((permission: string) => (
          <Badge key={permission} variant="secondary">
            {permission}
          </Badge>
        ))}
      </div>
      <div className="mt-2 flex items-center justify-between">
        <p className="text-sm text-muted-foreground">
          Total: {permissions.length} permission
          {permissions.length !== 1 ? 's' : ''}
        </p>
        {hasMore && (
          <Button
            variant="ghost"
            size="sm"
            className="h-auto p-0 text-sm"
            onClick={() => setShowAll(!showAll)}
          >
            {showAll ? 'Show less' : `Show ${permissions.length - 10} more`}
          </Button>
        )}
      </div>
    </>
  )
}

function apiKeyVerdict(apiKey: { is_active: boolean }): {
  tone: StatusTone
  label: string
} {
  return apiKey.is_active
    ? { tone: 'ok', label: 'Active' }
    : { tone: 'idle', label: 'Inactive' }
}

function apiKeyFacts(apiKey: {
  id: number
  role_type: string
  created_at: string
  last_used_at?: string | null
  expires_at?: string | null
}): DetailFact[] {
  return [
    { label: 'ID', value: <span className="font-mono">{apiKey.id}</span> },
    {
      label: 'Access level',
      value:
        apiKey.role_type === 'custom' ? 'Custom Permissions' : apiKey.role_type,
    },
    {
      label: 'Created',
      value: (
        <span title={fmtDateTime(apiKey.created_at)}>
          {fmtRelativeTime(apiKey.created_at)}
        </span>
      ),
    },
    {
      label: 'Last used',
      value: apiKey.last_used_at ? (
        <span title={fmtDateTime(apiKey.last_used_at)}>
          {fmtRelativeTime(apiKey.last_used_at)}
        </span>
      ) : (
        'Never'
      ),
    },
    {
      label: 'Expires',
      value: apiKey.expires_at ? (
        <span title={fmtDateTime(apiKey.expires_at)}>
          {fmtRelativeTime(apiKey.expires_at)}
        </span>
      ) : (
        'Never'
      ),
    },
  ]
}

function ApiKeyDetailSkeleton({ backAction }: { backAction: React.ReactNode }) {
  return (
    <Detail
      embedded
      title={<Skeleton className="h-7 w-48" />}
      actions={backAction}
      facts={[0, 1, 2, 3, 4].map(() => ({
        label: <Skeleton className="h-3 w-16" />,
        value: <Skeleton className="h-4 w-24" />,
      }))}
      main={
        <Card>
          <CardHeader>
            <Skeleton className="h-5 w-40" />
            <Skeleton className="mt-2 h-4 w-64" />
          </CardHeader>
          <CardContent className="space-y-4">
            <Skeleton className="h-14 w-full rounded-lg" />
            <Skeleton className="h-14 w-full rounded-lg" />
          </CardContent>
        </Card>
      }
    />
  )
}

export default function ApiKeyDetail() {
  usePageTitle('API Key Details')
  const navigate = useNavigate()
  const { id } = useParams<{ id: string }>()
  const queryClient = useQueryClient()

  const {
    data: apiKey,
    isLoading,
    error: apiKeyError,
    refetch: refetchApiKey,
  } = useQuery({
    queryKey: ['apiKey', id],
    queryFn: async () => {
      if (!id) throw new Error('API Key ID is required')
      const response = await getApiKey({ path: { id: parseInt(id) } })
      return response.data
    },
    enabled: !!id,
  })

  useEffect(() => {
    if (apiKey && apiKeyError) {
      toast.error('Failed to refresh API key', {
        action: { label: 'Retry', onClick: () => void refetchApiKey() },
      })
    }
  }, [apiKey, apiKeyError, refetchApiKey])

  const { data: permissionsData } = useApiKeyPermissions()

  const deleteMutation = useMutation({
    mutationFn: () => deleteApiKey({ path: { id: parseInt(id!) } }),
    meta: {
      errorTitle: 'Failed to delete API key',
    },
    onSuccess: () => {
      toast.success('API key deleted successfully')
      navigate('/settings/keys')
    },
  })

  const activateMutation = useMutation({
    mutationFn: () => activateApiKey({ path: { id: parseInt(id!) } }),
    meta: {
      errorTitle: 'Failed to activate API key',
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['apiKey', id] })
      queryClient.invalidateQueries({ queryKey: ['apiKeys'] })
      toast.success('API key activated')
    },
  })

  const deactivateMutation = useMutation({
    mutationFn: () => deactivateApiKey({ path: { id: parseInt(id!) } }),
    meta: {
      errorTitle: 'Failed to deactivate API key',
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['apiKey', id] })
      queryClient.invalidateQueries({ queryKey: ['apiKeys'] })
      toast.success('API key deactivated')
    },
  })

  const backAction = (
    <Button
      variant="ghost"
      size="sm"
      onClick={() => navigate('/settings/keys')}
    >
      <ArrowLeft className="mr-2 h-4 w-4" />
      Back to API Keys
    </Button>
  )

  if (isLoading) {
    return <ApiKeyDetailSkeleton backAction={backAction} />
  }

  const isNotFound =
    (apiKeyError as any)?.status === 404 ||
    (apiKeyError as any)?.title === 'API Key Not Found'

  if (!apiKey && apiKeyError && !isNotFound) {
    return (
      <PageState
        variant="failed"
        icon={AlertCircle}
        title="Failed to load API key"
        description={
          apiKeyError instanceof Error
            ? apiKeyError.message
            : 'An unexpected error occurred. Please try again.'
        }
        action={
          <div className="flex gap-2">
            <Button variant="outline" onClick={() => void refetchApiKey()}>
              Retry
            </Button>
            {backAction}
          </div>
        }
      />
    )
  }

  if (!apiKey) {
    return (
      <PageState
        variant="empty"
        icon={Key}
        title="API key not found"
        description="This API key may have been deleted, or you may not have permission to view it."
        action={backAction}
      />
    )
  }

  const verdict = apiKeyVerdict(apiKey)

  return (
    <Detail
      embedded
      title={apiKey.name}
      description="API key details and permissions"
      verdict={<Status tone={verdict.tone} label={verdict.label} />}
      actions={
        <>
          {backAction}
          {apiKey.is_active ? (
            <Button
              variant="outline"
              size="sm"
              onClick={() => deactivateMutation.mutate()}
              busy={deactivateMutation.isPending}
              busyLabel="Deactivating…"
            >
              <X className="mr-2 h-4 w-4" />
              Deactivate
            </Button>
          ) : (
            <Button
              variant="outline"
              size="sm"
              onClick={() => activateMutation.mutate()}
              busy={activateMutation.isPending}
              busyLabel="Activating…"
            >
              <Check className="mr-2 h-4 w-4" />
              Activate
            </Button>
          )}
          <AlertDialog>
            <AlertDialogTrigger asChild>
              <Button variant="destructive" size="sm">
                <Trash2 className="mr-2 h-4 w-4" />
                Delete
              </Button>
            </AlertDialogTrigger>
            <AlertDialogContent>
              <AlertDialogHeader>
                <AlertDialogTitle>Delete API Key</AlertDialogTitle>
                <AlertDialogDescription>
                  Are you sure you want to delete &quot;{apiKey.name}&quot;?
                  This action cannot be undone and will immediately invalidate
                  all requests using this key.
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel>Cancel</AlertDialogCancel>
                <AlertDialogAction
                  className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
                  disabled={deleteMutation.isPending}
                  onClick={() => deleteMutation.mutate()}
                >
                  {deleteMutation.isPending ? 'Deleting...' : 'Delete'}
                </AlertDialogAction>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>
        </>
      }
      facts={apiKeyFacts(apiKey)}
      main={
        <>
          {!apiKey.is_active && (
            <Callout tone="warning" title="This API key is inactive">
              It cannot be used for authentication until reactivated.
            </Callout>
          )}

          <Card>
            <CardHeader>
              <CardTitle className="flex items-center gap-2">
                <Shield className="h-5 w-5" />
                Permissions & Access
              </CardTitle>
              <CardDescription>
                Current permissions and access level
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-6">
              <div>
                <Label className="text-muted-foreground">Permissions</Label>
                <p className="mb-2 mt-1 text-xs text-muted-foreground">
                  Permissions cannot be changed after creation. To change
                  permissions, delete this key and create a new one.
                </p>
                <PermissionsDisplay
                  permissions={
                    apiKey.role_type === 'custom' && apiKey.permissions
                      ? apiKey.permissions
                      : permissionsData?.roles.find(
                          (r) => r.name === apiKey.role_type
                        )?.permissions || []
                  }
                />
              </div>
            </CardContent>
          </Card>
        </>
      }
    />
  )
}
