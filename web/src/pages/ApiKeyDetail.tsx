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
import { Checkbox } from '@/components/ui/checkbox'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
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
  updateApiKey,
  deleteApiKey,
  activateApiKey,
  deactivateApiKey,
} from '@/api/client'
import { useApiKeyPermissions } from '@/components/api-keys/useApiKeyPermissions'
import {
  ArrowLeft,
  Edit,
  Shield,
  Key,
  Calendar,
  Activity,
  AlertTriangle,
  Check,
  X,
  Save,
  RotateCcw,
  ChevronRight,
} from 'lucide-react'
import { format } from 'date-fns'
import { toast } from 'sonner'
import { cn } from '@/lib/utils'

export default function ApiKeyDetail() {
  const navigate = useNavigate()
  const { id } = useParams<{ id: string }>()
  const queryClient = useQueryClient()
  const [isEditing, setIsEditing] = useState(false)
  const [deleteModalOpen, setDeleteModalOpen] = useState(false)
  const [selectedRole, setSelectedRole] = useState<string>('')
  const [selectedPermissions, setSelectedPermissions] = useState<Set<string>>(
    new Set()
  )
  const [useCustomPermissions, setUseCustomPermissions] = useState(false)
  const [openCategories, setOpenCategories] = useState<Set<string>>(new Set())

  const { data: apiKey, isLoading } = useQuery({
    queryKey: ['apiKey', id],
    queryFn: async () => {
      if (!id) throw new Error('API Key ID is required')
      const response = await getApiKey({ path: { id: parseInt(id) } })
      return response.data
    },
    enabled: !!id,
  })

  const { data: permissionsData, isLoading: isLoadingPermissions } =
    useApiKeyPermissions()

  const updateMutation = useMutation({
    mutationFn: (data: any) =>
      updateApiKey({ path: { id: parseInt(id!) }, body: data }),
    meta: {
      errorTitle: 'Failed to update API key',
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['apiKey', id] })
      queryClient.invalidateQueries({ queryKey: ['apiKeys'] })
      setIsEditing(false)
      toast.success('API key updated successfully')
    },
  })

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

  const handleRoleSelect = (role: string) => {
    setSelectedRole(role)
    setUseCustomPermissions(false)
    const selectedRoleInfo = permissionsData?.roles.find(
      (r: any) => r.name === role
    )
    if (selectedRoleInfo?.permissions) {
      setSelectedPermissions(new Set(selectedRoleInfo.permissions))
    }
  }

  const togglePermission = (permission: string) => {
    const newPermissions = new Set(selectedPermissions)
    if (newPermissions.has(permission)) {
      newPermissions.delete(permission)
    } else {
      newPermissions.add(permission)
    }
    setSelectedPermissions(newPermissions)
    setUseCustomPermissions(true)
    setSelectedRole('')
  }

  const handleEditStart = () => {
    if (apiKey) {
      setSelectedRole(apiKey.role_type || '')
      setSelectedPermissions(new Set(apiKey.permissions || []))
      setUseCustomPermissions(apiKey.role_type === 'custom')
      setIsEditing(true)
    }
  }

  const handleSave = () => {
    const data = {
      role_type: useCustomPermissions ? 'custom' : selectedRole,
      permissions: useCustomPermissions
        ? Array.from(selectedPermissions)
        : undefined,
    }
    updateMutation.mutate(data)
  }

  const handleCancel = () => {
    setIsEditing(false)
    setSelectedRole('')
    setSelectedPermissions(new Set())
    setUseCustomPermissions(false)
    setOpenCategories(new Set())
  }

  const toggleCategory = (category: string) => {
    const newOpenCategories = new Set(openCategories)
    if (newOpenCategories.has(category)) {
      newOpenCategories.delete(category)
    } else {
      newOpenCategories.add(category)
    }
    setOpenCategories(newOpenCategories)
  }

  const toggleCategoryPermissions = (
    category: string,
    categoryPermissions: any[]
  ) => {
    const categoryPermissionNames = categoryPermissions.map((p) => p.name)
    const allSelected = categoryPermissionNames.every((name) =>
      selectedPermissions.has(name)
    )

    const newPermissions = new Set(selectedPermissions)

    if (allSelected) {
      // Deselect all permissions in this category
      categoryPermissionNames.forEach((name) => newPermissions.delete(name))
    } else {
      // Select all permissions in this category
      categoryPermissionNames.forEach((name) => newPermissions.add(name))
    }

    setSelectedPermissions(newPermissions)
    setUseCustomPermissions(true)
    setSelectedRole('')
  }

  // Group permissions by category
  const permissionsByCategory = permissionsData?.permissions.reduce(
    (acc: any, perm: any) => {
      if (!acc[perm.category]) {
        acc[perm.category] = []
      }
      acc[perm.category].push(perm)
      return acc
    },
    {} as Record<string, any[]>
  )

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
          <div className="flex items-center justify-between">
            <div>
              <CardTitle className="flex items-center gap-2">
                <Shield className="h-5 w-5" />
                Permissions & Access
              </CardTitle>
              <CardDescription>
                {isEditing
                  ? 'Edit permissions and access level'
                  : 'Current permissions and access level'}
              </CardDescription>
            </div>
            <div className="flex items-center gap-2">
              {isEditing ? (
                <>
                  <Button variant="outline" size="sm" onClick={handleCancel}>
                    <RotateCcw className="h-4 w-4 mr-2" />
                    Cancel
                  </Button>
                  <Button
                    size="sm"
                    onClick={handleSave}
                    disabled={
                      updateMutation.isPending ||
                      (!selectedRole && selectedPermissions.size === 0)
                    }
                  >
                    <Save className="h-4 w-4 mr-2" />
                    {updateMutation.isPending ? 'Saving...' : 'Save'}
                  </Button>
                </>
              ) : (
                <Button variant="outline" size="sm" onClick={handleEditStart}>
                  <Edit className="h-4 w-4 mr-2" />
                  Edit
                </Button>
              )}
            </div>
          </div>
        </CardHeader>
        <CardContent className="space-y-6">
          {!isEditing ? (
            // View Mode
            <>
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
                {(() => {
                  // Get role-based permissions if not custom
                  const rolePermissions =
                    apiKey.role_type !== 'custom'
                      ? permissionsData?.roles.find(
                          (r) => r.name === apiKey.role_type
                        )?.permissions || []
                      : []

                  // Use custom permissions or role permissions
                  const allPermissions =
                    apiKey.role_type === 'custom' && apiKey.permissions
                      ? apiKey.permissions
                      : rolePermissions

                  const [showAll, setShowAll] = useState(false)
                  const displayPermissions = showAll
                    ? allPermissions
                    : allPermissions.slice(0, 10)
                  const hasMore = allPermissions.length > 10

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
                          Total: {allPermissions.length} permission
                          {allPermissions.length !== 1 ? 's' : ''}
                        </p>
                        {hasMore && (
                          <Button
                            variant="ghost"
                            size="sm"
                            className="text-sm h-auto p-0"
                            onClick={() => setShowAll(!showAll)}
                          >
                            {showAll
                              ? 'Show less'
                              : `Show ${allPermissions.length - 10} more`}
                          </Button>
                        )}
                      </div>
                    </>
                  )
                })()}
              </div>
            </>
          ) : (
            // Edit Mode
            <div className="space-y-6">
              {isLoadingPermissions ? (
                <div className="text-center py-8">Loading permissions...</div>
              ) : (
                <>
                  {/* Predefined Roles */}
                  <div>
                    <Label className="text-sm font-medium mb-3 block">
                      Choose Access Level
                    </Label>
                    <div className="grid gap-3">
                      {permissionsData?.roles.map((role: any) => (
                        <div
                          key={role.name}
                          className={cn(
                            'p-4 rounded-lg border-2 cursor-pointer transition-colors',
                            selectedRole === role.name
                              ? 'border-primary bg-primary/5'
                              : 'border-border hover:border-primary/50'
                          )}
                          onClick={() => handleRoleSelect(role.name)}
                        >
                          <div className="flex items-start justify-between">
                            <div className="space-y-1">
                              <div className="font-medium">{role.name}</div>
                              <div className="text-sm text-muted-foreground">
                                {role.description}
                              </div>
                              <div className="flex flex-wrap gap-1 mt-2">
                                {role.permissions
                                  .slice(0, 5)
                                  .map((p: string) => (
                                    <Badge
                                      key={p}
                                      variant="secondary"
                                      className="text-xs"
                                    >
                                      {p}
                                    </Badge>
                                  ))}
                                {role.permissions.length > 5 && (
                                  <Badge variant="outline" className="text-xs">
                                    +{role.permissions.length - 5} more
                                  </Badge>
                                )}
                              </div>
                            </div>
                            <div className="mt-1">
                              <div
                                className={cn(
                                  'w-5 h-5 rounded-full border-2',
                                  selectedRole === role.name
                                    ? 'border-primary bg-primary'
                                    : 'border-muted-foreground'
                                )}
                              >
                                {selectedRole === role.name && (
                                  <Check className="h-3 w-3 text-primary-foreground m-auto" />
                                )}
                              </div>
                            </div>
                          </div>
                        </div>
                      ))}
                    </div>
                  </div>

                  <Separator />

                  {/* Custom Permissions */}
                  <div>
                    <Label className="text-sm font-medium mb-3 block">
                      Or Select Custom Permissions
                    </Label>
                    {permissionsByCategory &&
                      Object.entries(permissionsByCategory).map(
                        ([category, perms]) => {
                          const categoryPermissions = perms as any[]
                          const allSelected = categoryPermissions.every((p) =>
                            selectedPermissions.has(p.name)
                          )
                          const someSelected = categoryPermissions.some((p) =>
                            selectedPermissions.has(p.name)
                          )

                          return (
                            <Collapsible
                              key={category}
                              open={openCategories.has(category)}
                              onOpenChange={() => toggleCategory(category)}
                              className="mb-4"
                            >
                              <div className="border rounded-lg">
                                <CollapsibleTrigger asChild>
                                  <div className="flex items-center justify-between p-4 hover:bg-muted/50 cursor-pointer">
                                    <div className="flex items-center gap-3">
                                      <Checkbox
                                        checked={allSelected}
                                        ref={(ref) => {
                                          if (ref) {
                                            const element = ref as any
                                            element.indeterminate =
                                              someSelected && !allSelected
                                          }
                                        }}
                                        onCheckedChange={() => {
                                          toggleCategoryPermissions(
                                            category,
                                            categoryPermissions
                                          )
                                        }}
                                        onClick={(e) => e.stopPropagation()}
                                      />
                                      <div>
                                        <h4 className="font-medium text-sm">
                                          {category}
                                        </h4>
                                        <p className="text-xs text-muted-foreground">
                                          {categoryPermissions.length}{' '}
                                          permission
                                          {categoryPermissions.length !== 1
                                            ? 's'
                                            : ''}
                                          {someSelected &&
                                            ` • ${categoryPermissions.filter((p) => selectedPermissions.has(p.name)).length} selected`}
                                        </p>
                                      </div>
                                    </div>
                                    <ChevronRight
                                      className={cn(
                                        'h-4 w-4 transition-transform',
                                        openCategories.has(category) &&
                                          'rotate-90'
                                      )}
                                    />
                                  </div>
                                </CollapsibleTrigger>
                                <CollapsibleContent>
                                  <div className="border-t p-4 space-y-3">
                                    {categoryPermissions.map((perm: any) => (
                                      <div
                                        key={perm.name}
                                        className="flex items-start space-x-3 p-2 rounded-lg hover:bg-muted/50"
                                      >
                                        <Checkbox
                                          id={perm.name}
                                          checked={selectedPermissions.has(
                                            perm.name
                                          )}
                                          onCheckedChange={() =>
                                            togglePermission(perm.name)
                                          }
                                        />
                                        <div className="flex-1">
                                          <label
                                            htmlFor={perm.name}
                                            className="text-sm font-medium leading-none cursor-pointer"
                                          >
                                            {perm.name}
                                          </label>
                                          <p className="text-xs text-muted-foreground mt-1">
                                            {perm.description}
                                          </p>
                                        </div>
                                      </div>
                                    ))}
                                  </div>
                                </CollapsibleContent>
                              </div>
                            </Collapsible>
                          )
                        }
                      )}
                  </div>
                </>
              )}
            </div>
          )}
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
              Are you sure you want to delete &quot;{apiKey.name}&quot;? This action
              cannot be undone and will immediately invalidate all requests
              using this key.
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
