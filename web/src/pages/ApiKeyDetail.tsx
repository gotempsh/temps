import { useState } from 'react'
import { useNavigate, useParams } from 'react-router-dom'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Separator } from '@/components/ui/separator'
import { Label } from '@/components/ui/label'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  getApiKey,
  deleteApiKey,
  activateApiKey,
  deactivateApiKey,
} from '@/api/client'
import { useApiKeyPermissions } from '@/components/api-keys/useApiKeyPermissions'
import {
  ArrowLeft,
  Shield,
  Key,
  Calendar,
  Activity,
  AlertTriangle,
  Check,
  X,
} from 'lucide-react'
import { format } from 'date-fns'
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
      <div className="flex flex-wrap gap-2 mt-2">
        {displayPermissions.map((permission: string) => (
          <Badge key={permission} variant="secondary">
            {permission}
          </Badge>
        ))}
      </div>
      <div className="flex items-center justify-between mt-2">
        <p className="text-sm text-muted-foreground">
          Total: {permissions.length} permission
          {permissions.length !== 1 ? 's' : ''}
        </p>
        {hasMore && (
          <Button
            variant="ghost"
            size="sm"
            className="text-sm h-auto p-0"
            onClick={() => setShowAll(!showAll)}
          >
            {showAll ? 'Show less' : `Show ${permissions.length - 10} more`}
          </Button>
        )}
      </div>
    </>
  )
}

export default function ApiKeyDetail() {
  const navigate = useNavigate()
  const { id } = useParams<{ id: string }>()
  const queryClient = useQueryClient()
  const [deleteModalOpen, setDeleteModalOpen] = useState(false)

  const { data: apiKey, isLoading } = useQuery({
    queryKey: ['apiKey', id],
    queryFn: async () => {
      if (!id) throw new Error('API Key ID is required')
      const response = await getApiKey({ path: { id: parseInt(id) } })
      return response.data
    },
    enabled: !!id,
  })

  const { data: permissionsData } = useApiKeyPermissions()

  const deleteMutation = useMutation({
    mutationFn: () => deleteApiKey({ path: { id: parseInt(id!) } }),
    meta: {
      errorTitle: 'Failed to delete API key',
    },
    onSuccess: () => {
      toast.success('API key deleted successfully')
      navigate('/keys')
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

  if (isLoading) {
    return (
      <div className="container max-w-4xl mx-auto py-6">
        <div className="text-center py-8">Loading API key details...</div>
      </div>
    )
  }

  if (!apiKey) {
    return (
      <div className="container max-w-4xl mx-auto py-6">
        <div className="text-center py-8">
          <p>API key not found</p>
          <Button onClick={() => navigate('/keys')} className="mt-4">
            Back to API Keys
          </Button>
        </div>
      </div>
    )
  }

  return (
    <div className="container max-w-4xl mx-auto py-6 space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-4">
          <Button variant="ghost" size="icon" onClick={() => navigate('/keys')}>
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <div>
            <h1 className="text-3xl font-bold">{apiKey.name}</h1>
            <p className="text-muted-foreground mt-1">
              API key details and permissions
            </p>
          </div>
        </div>
        <div className="flex items-center gap-2">
          <Badge variant={apiKey.is_active ? 'default' : 'secondary'}>
            {apiKey.is_active ? 'Active' : 'Inactive'}
          </Badge>
        </div>
      </div>

      {/* Status Alert */}
      {!apiKey.is_active && (
        <Alert>
          <AlertTriangle className="h-4 w-4" />
          <AlertDescription>
            This API key is currently inactive and cannot be used for
            authentication.
          </AlertDescription>
        </Alert>
      )}

      {/* Basic Information */}
      <Card>
        <CardHeader>
          <div className="flex items-center justify-between">
            <div>
              <CardTitle className="flex items-center gap-2">
                <Key className="h-5 w-5" />
                Basic Information
              </CardTitle>
              <CardDescription>Overview of your API key</CardDescription>
            </div>
            <div className="flex items-center gap-2">
              {apiKey.is_active ? (
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => deactivateMutation.mutate()}
                  disabled={deactivateMutation.isPending}
                >
                  <X className="h-4 w-4 mr-2" />
                  Deactivate
                </Button>
              ) : (
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => activateMutation.mutate()}
                  disabled={activateMutation.isPending}
                >
                  <Check className="h-4 w-4 mr-2" />
                  Activate
                </Button>
              )}
            </div>
          </div>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="grid gap-4 md:grid-cols-2">
            <div>
              <Label className="text-muted-foreground">Name</Label>
              <p className="font-medium">{apiKey.name}</p>
            </div>
            <div>
              <Label className="text-muted-foreground">ID</Label>
              <p className="font-mono text-sm">{apiKey.id}</p>
            </div>
            <div>
              <Label className="text-muted-foreground">Created</Label>
              <p className="flex items-center gap-2">
                <Calendar className="h-4 w-4" />
                {format(new Date(apiKey.created_at), 'MMM d, yyyy HH:mm')}
              </p>
            </div>
            <div>
              <Label className="text-muted-foreground">Last Used</Label>
              <p className="flex items-center gap-2">
                <Activity className="h-4 w-4" />
                {apiKey.last_used_at
                  ? format(new Date(apiKey.last_used_at), 'MMM d, yyyy HH:mm')
                  : 'Never'}
              </p>
            </div>
            {apiKey.expires_at && (
              <div className="md:col-span-2">
                <Label className="text-muted-foreground">Expires</Label>
                <p className="flex items-center gap-2">
                  <Calendar className="h-4 w-4" />
                  {format(new Date(apiKey.expires_at), 'MMM d, yyyy HH:mm')}
                </p>
              </div>
            )}
          </div>
        </CardContent>
      </Card>

      {/* Permissions */}
      <Card>
        <CardHeader>
          <div>
            <CardTitle className="flex items-center gap-2">
              <Shield className="h-5 w-5" />
              Permissions & Access
            </CardTitle>
            <CardDescription>
              Current permissions and access level
            </CardDescription>
          </div>
        </CardHeader>
        <CardContent className="space-y-6">
          <div>
            <Label className="text-muted-foreground">Access Level</Label>
            <p className="font-medium mt-1">
              {apiKey.role_type === 'custom'
                ? 'Custom Permissions'
                : apiKey.role_type}
            </p>
          </div>

          <Separator />

          <div>
            <Label className="text-muted-foreground">Permissions</Label>
            <p className="text-xs text-muted-foreground mt-1 mb-2">
              Permissions cannot be changed after creation. To change permissions, delete this key and create a new one.
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

      {/* Danger Zone */}
      <Card>
        <CardHeader>
          <CardTitle className="text-destructive">Danger Zone</CardTitle>
          <CardDescription>
            Irreversible and destructive actions
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="flex items-center justify-between p-4 border border-destructive/20 rounded-lg">
            <div>
              <h4 className="font-medium">Delete API Key</h4>
              <p className="text-sm text-muted-foreground">
                Once deleted, this API key cannot be recovered and will stop
                working immediately.
              </p>
            </div>
            <Button
              variant="destructive"
              onClick={() => setDeleteModalOpen(true)}
            >
              Delete Key
            </Button>
          </div>
        </CardContent>
      </Card>

      {/* Delete Confirmation Modal */}
      <Dialog open={deleteModalOpen} onOpenChange={setDeleteModalOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Delete API Key</DialogTitle>
            <DialogDescription>
              Are you sure you want to delete &quot;{apiKey.name}&quot;? This
              action cannot be undone and will immediately invalidate all
              requests using this key.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteModalOpen(false)}>
              Cancel
            </Button>
            <Button
              variant="destructive"
              onClick={() => deleteMutation.mutate()}
              disabled={deleteMutation.isPending}
            >
              {deleteMutation.isPending ? 'Deleting...' : 'Delete'}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}
