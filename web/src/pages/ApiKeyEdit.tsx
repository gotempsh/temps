import { useEffect, useState } from 'react'
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
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'
import { Badge } from '@/components/ui/badge'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Separator } from '@/components/ui/separator'
import { Skeleton } from '@/components/ui/skeleton'
import {
  ArrowLeft,
  Shield,
  Calendar,
  Clock,
  AlertCircle,
  Activity,
} from 'lucide-react'
import { toast } from 'sonner'
import { format } from 'date-fns'
import { getApiKey, updateApiKey, type UpdateApiKeyRequest } from '@/api/client'

export default function ApiKeyEdit() {
  const navigate = useNavigate()
  const { id } = useParams<{ id: string }>()
  const queryClient = useQueryClient()

  const [formData, setFormData] = useState({
    name: '',
    is_active: true,
    expires_at: '',
  })

  const { data: apiKey, isLoading } = useQuery({
    queryKey: ['apiKey', id],
    queryFn: async () => {
      if (!id) throw new Error('No API key ID provided')
      const response = await getApiKey({ path: { id: parseInt(id) } })
      return response.data
    },
    enabled: !!id,
  })

  useEffect(() => {
    if (apiKey) {
      setFormData({
        name: apiKey.name,
        is_active: apiKey.is_active,
        expires_at: apiKey.expires_at
          ? new Date(apiKey.expires_at).toISOString().split('T')[0]
          : '',
      })
    }
  }, [apiKey])

  const updateMutation = useMutation({
    mutationFn: (data: UpdateApiKeyRequest) =>
      updateApiKey({ path: { id: parseInt(id!) }, body: data }),
    meta: {
      errorTitle: 'Failed to update API key',
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['apiKeys'] })
      queryClient.invalidateQueries({ queryKey: ['apiKey', id] })
      toast.success('API key updated successfully')
      navigate('/keys')
    },
  })

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()
    const data: UpdateApiKeyRequest = {
      name: formData.name,
      is_active: formData.is_active,
      expires_at: formData.expires_at
        ? new Date(formData.expires_at).toISOString()
        : null,
    }
    updateMutation.mutate(data)
  }

  if (isLoading) {
    return (
      <div className="container max-w-4xl mx-auto py-6 space-y-6">
        <div className="flex items-center gap-4">
          <Skeleton className="h-10 w-10" />
          <div className="space-y-2">
            <Skeleton className="h-8 w-48" />
            <Skeleton className="h-4 w-64" />
          </div>
        </div>
        <Card>
          <CardHeader>
            <Skeleton className="h-6 w-32" />
            <Skeleton className="h-4 w-48" />
          </CardHeader>
          <CardContent className="space-y-6">
            <Skeleton className="h-10 w-full" />
            <Skeleton className="h-10 w-full" />
            <Skeleton className="h-20 w-full" />
          </CardContent>
        </Card>
      </div>
    )
  }

  if (!apiKey) {
    return (
      <div className="container max-w-4xl mx-auto py-6">
        <Card>
          <CardContent className="py-12 text-center">
            <AlertCircle className="h-12 w-12 text-muted-foreground mx-auto mb-4" />
            <h3 className="text-lg font-medium">API Key not found</h3>
            <p className="text-muted-foreground mt-2">
              The API key you're looking for doesn't exist or has been deleted.
            </p>
            <Button className="mt-4" onClick={() => navigate('/keys')}>
              Back to API Keys
            </Button>
          </CardContent>
        </Card>
      </div>
    )
  }

  const isExpired =
    apiKey.expires_at && new Date(apiKey.expires_at) < new Date()

  return (
    <div className="container max-w-4xl mx-auto py-6 space-y-6">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-4">
          <Button variant="ghost" size="icon" onClick={() => navigate('/keys')}>
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <div>
            <h1 className="text-3xl font-bold">Edit API Key</h1>
            <p className="text-muted-foreground mt-1">
              Update settings for your API key
            </p>
          </div>
        </div>
      </div>

      <form onSubmit={handleSubmit} className="space-y-6">
        {/* Key Information Card */}
        <Card>
          <CardHeader>
            <CardTitle>Key Information</CardTitle>
            <CardDescription>
              View and manage your API key details
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-6">
            {/* Read-only Information */}
            <div className="grid gap-4 p-4 bg-muted/50 rounded-lg">
              <div className="grid grid-cols-2 gap-4">
                <div>
                  <Label className="text-muted-foreground text-xs">
                    Key Prefix
                  </Label>
                  <p className="font-mono text-sm mt-1">
                    {apiKey.key_prefix}...
                  </p>
                </div>
                <div>
                  <Label className="text-muted-foreground text-xs">
                    Created
                  </Label>
                  <p className="text-sm mt-1">
                    {format(new Date(apiKey.created_at), 'PPP')}
                  </p>
                </div>
                <div>
                  <Label className="text-muted-foreground text-xs">
                    Last Used
                  </Label>
                  <p className="text-sm mt-1">
                    {apiKey.last_used_at
                      ? format(new Date(apiKey.last_used_at), 'PPP')
                      : 'Never'}
                  </p>
                </div>
                <div>
                  <Label className="text-muted-foreground text-xs">Role</Label>
                  <Badge variant="outline" className="mt-1">
                    {apiKey.role_type}
                  </Badge>
                </div>
              </div>
            </div>

            <Separator />

            {/* Editable Fields */}
            <div className="space-y-4">
              <div className="space-y-2">
                <Label htmlFor="name">Name</Label>
                <Input
                  id="name"
                  value={formData.name}
                  onChange={(e) =>
                    setFormData({ ...formData, name: e.target.value })
                  }
                  required
                />
                <p className="text-sm text-muted-foreground">
                  A descriptive name to help you identify this key
                </p>
              </div>

              <div className="flex items-center justify-between">
                <div className="space-y-1">
                  <Label
                    htmlFor="is_active"
                    className="flex items-center gap-2"
                  >
                    <Activity className="h-4 w-4" />
                    Active Status
                  </Label>
                  <p className="text-sm text-muted-foreground">
                    Disable this key to temporarily revoke access
                  </p>
                </div>
                <Switch
                  id="is_active"
                  checked={formData.is_active}
                  onCheckedChange={(checked) =>
                    setFormData({ ...formData, is_active: checked })
                  }
                />
              </div>

              <div className="space-y-2">
                <Label htmlFor="expires_at" className="flex items-center gap-2">
                  <Calendar className="h-4 w-4" />
                  Expiration Date
                </Label>
                <Input
                  id="expires_at"
                  type="date"
                  value={formData.expires_at}
                  onChange={(e) =>
                    setFormData({ ...formData, expires_at: e.target.value })
                  }
                  min={new Date().toISOString().split('T')[0]}
                />
                <p className="text-sm text-muted-foreground">
                  Leave empty for no expiration. Current keys expire on the set
                  date.
                </p>
                {isExpired && (
                  <Alert variant="destructive">
                    <AlertCircle className="h-4 w-4" />
                    <AlertDescription>
                      This key has expired and can no longer be used.
                    </AlertDescription>
                  </Alert>
                )}
              </div>
            </div>
          </CardContent>
        </Card>

        {/* Permissions Card */}
        {apiKey.permissions && apiKey.permissions.length > 0 && (
          <Card>
            <CardHeader>
              <CardTitle className="flex items-center gap-2">
                <Shield className="h-5 w-5" />
                Permissions
              </CardTitle>
              <CardDescription>
                Permissions cannot be changed after creation
              </CardDescription>
            </CardHeader>
            <CardContent>
              <div className="space-y-4">
                {apiKey.role_type !== 'custom' ? (
                  <div className="p-4 bg-muted/50 rounded-lg">
                    <p className="text-sm font-medium mb-2">
                      Role: {apiKey.role_type}
                    </p>
                    <p className="text-xs text-muted-foreground">
                      This key uses a predefined role with the following
                      permissions:
                    </p>
                  </div>
                ) : (
                  <div className="p-4 bg-muted/50 rounded-lg">
                    <p className="text-sm font-medium">Custom Permissions</p>
                    <p className="text-xs text-muted-foreground">
                      This key has custom permissions configured
                    </p>
                  </div>
                )}

                <div className="flex flex-wrap gap-2">
                  {apiKey.permissions.map((permission) => (
                    <Badge key={permission} variant="secondary">
                      {permission}
                    </Badge>
                  ))}
                </div>

                <p className="text-sm text-muted-foreground">
                  Total: {apiKey.permissions.length} permission
                  {apiKey.permissions.length !== 1 ? 's' : ''}
                </p>
              </div>
            </CardContent>
          </Card>
        )}

        {/* Usage Statistics Card */}
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <Clock className="h-5 w-5" />
              Usage Information
            </CardTitle>
            <CardDescription>
              Track when and how this API key is being used
            </CardDescription>
          </CardHeader>
          <CardContent>
            <div className="grid gap-4">
              <div className="grid grid-cols-2 gap-4">
                <div className="space-y-1">
                  <Label className="text-muted-foreground text-xs">
                    Status
                  </Label>
                  <div>
                    {formData.is_active ? (
                      <Badge variant="default">Active</Badge>
                    ) : (
                      <Badge variant="secondary">Inactive</Badge>
                    )}
                    {isExpired && (
                      <Badge variant="destructive" className="ml-2">
                        Expired
                      </Badge>
                    )}
                  </div>
                </div>
                <div className="space-y-1">
                  <Label className="text-muted-foreground text-xs">
                    Last Activity
                  </Label>
                  <p className="text-sm">
                    {apiKey.last_used_at
                      ? `${format(new Date(apiKey.last_used_at), 'PPp')}`
                      : 'No activity recorded'}
                  </p>
                </div>
              </div>
            </div>
          </CardContent>
        </Card>

        {/* Action Buttons */}
        <div className="flex justify-between">
          <Button
            type="button"
            variant="outline"
            onClick={() => navigate('/keys')}
          >
            Cancel
          </Button>
          <Button type="submit" disabled={updateMutation.isPending}>
            {updateMutation.isPending ? 'Saving...' : 'Save Changes'}
          </Button>
        </div>
      </form>
    </div>
  )
}
