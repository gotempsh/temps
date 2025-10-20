import { type CreateApiKeyRequest } from '@/api/client'
import { createApiKeyMutation } from '@/api/client/@tanstack/react-query.gen'
import { useApiKeyPermissions } from '@/components/api-keys/useApiKeyPermissions'
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
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Separator } from '@/components/ui/separator'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import { cn } from '@/lib/utils'
import { useMutation } from '@tanstack/react-query'
import {
  AlertCircle,
  ArrowLeft,
  Calendar,
  Check,
  Copy,
  Key,
  Shield,
  ChevronRight,
  Edit,
} from 'lucide-react'
import { useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'

export default function ApiKeyCreate() {
  const navigate = useNavigate()
  const [step, setStep] = useState(1)
  const [keyName, setKeyName] = useState('')
  const [expiresAt, setExpiresAt] = useState('')
  const [selectedRole, setSelectedRole] = useState<string>('')
  const [selectedPermissions, setSelectedPermissions] = useState<Set<string>>(
    new Set()
  )
  const [useCustomPermissions, setUseCustomPermissions] = useState(false)
  const [newKeySecret, setNewKeySecret] = useState<string | null>(null)
  const [copiedKey, setCopiedKey] = useState(false)
  const [openCategories, setOpenCategories] = useState<Set<string>>(new Set())
  const [createdKeyId, setCreatedKeyId] = useState<number | null>(null)

  const { data: permissionsData, isLoading: isLoadingPermissions } =
    useApiKeyPermissions()

  const createMutation = useMutation({
    ...createApiKeyMutation(),
    meta: {
      errorTitle: 'Failed to create API key',
    },
    onSuccess: (response) => {
      setNewKeySecret(response.api_key)
      setCreatedKeyId(response.id)
      setStep(4) // Show success step
      toast.success('API key created successfully')
    },
  })

  const handleCopyKey = async () => {
    if (newKeySecret) {
      await navigator.clipboard.writeText(newKeySecret)
      setCopiedKey(true)
      setTimeout(() => setCopiedKey(false), 2000)
    }
  }

  const handleRoleSelect = (role: string) => {
    setSelectedRole(role)
    setUseCustomPermissions(false)
    const selectedRoleInfo = permissionsData?.roles.find((r) => r.name === role)
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

  const handleSubmit = () => {
    const data: CreateApiKeyRequest = {
      name: keyName,
      role_type: useCustomPermissions ? 'custom' : selectedRole,
      permissions: useCustomPermissions
        ? Array.from(selectedPermissions)
        : undefined,
      expires_at: expiresAt ? new Date(expiresAt).toISOString() : undefined,
    }
    createMutation.mutate({
      body: data,
    })
  }

  const canProceed = () => {
    if (step === 1) return keyName.trim().length > 0
    if (step === 2)
      return (
        selectedRole || (useCustomPermissions && selectedPermissions.size > 0)
      )
    return true
  }

  // Group permissions by category
  const permissionsByCategory = permissionsData?.permissions.reduce(
    (acc, perm) => {
      if (!acc[perm.category]) {
        acc[perm.category] = []
      }
      acc[perm.category].push(perm)
      return acc
    },
    {} as Record<string, typeof permissionsData.permissions>
  )

  if (newKeySecret) {
    return (
      <div className="container max-w-2xl mx-auto py-6 space-y-6">
        <Card>
          <CardHeader>
            <div className="flex items-center justify-between">
              <div className="space-y-1">
                <CardTitle className="text-2xl">
                  API Key Created Successfully
                </CardTitle>
                <CardDescription>
                  Your API key has been created. Copy it now as it won't be
                  shown again.
                </CardDescription>
              </div>
              <div className="rounded-full bg-green-100 dark:bg-green-900/20 p-3">
                <Check className="h-6 w-6 text-green-600 dark:text-green-400" />
              </div>
            </div>
          </CardHeader>
          <CardContent className="space-y-6">
            <Alert>
              <AlertCircle className="h-4 w-4" />
              <AlertDescription>
                Make sure to copy your API key now. For security reasons, we
                won't show it again.
              </AlertDescription>
            </Alert>

            <div className="space-y-2">
              <Label>Your API Key</Label>
              <div className="flex gap-2">
                <Input
                  value={newKeySecret}
                  readOnly
                  className="font-mono text-sm"
                />
                <Button size="icon" variant="outline" onClick={handleCopyKey}>
                  {copiedKey ? (
                    <Check className="h-4 w-4" />
                  ) : (
                    <Copy className="h-4 w-4" />
                  )}
                </Button>
              </div>
            </div>

            <div className="space-y-4 p-4 bg-muted rounded-lg">
              <div className="text-sm">
                <strong>Name:</strong> {keyName}
              </div>
              <div className="text-sm">
                <strong>Access Level:</strong>{' '}
                {useCustomPermissions
                  ? `Custom (${selectedPermissions.size} permissions)`
                  : selectedRole}
              </div>
              {expiresAt && (
                <div className="text-sm">
                  <strong>Expires:</strong>{' '}
                  {new Date(expiresAt).toLocaleDateString()}
                </div>
              )}
            </div>

            <div className="flex justify-end gap-3">
              {createdKeyId && (
                <Button
                  variant="outline"
                  onClick={() => navigate(`/keys/${createdKeyId}`)}
                >
                  <Edit className="mr-2 h-4 w-4" />
                  Edit Permissions
                </Button>
              )}
              <Button onClick={() => navigate('/keys')}>Go to API Keys</Button>
            </div>
          </CardContent>
        </Card>
      </div>
    )
  }

  return (
    <div className="container max-w-4xl mx-auto py-6 space-y-6">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-4">
          <Button variant="ghost" size="icon" onClick={() => navigate('/keys')}>
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <div>
            <h1 className="text-3xl font-bold">Create API Key</h1>
            <p className="text-muted-foreground mt-1">
              Generate a new API key for programmatic access
            </p>
          </div>
        </div>
      </div>

      {/* Progress Steps */}
      <div className="flex items-center justify-center mb-8">
        {[
          { number: 1, name: 'Basic Info' },
          { number: 2, name: 'Permissions' },
          { number: 3, name: 'Review' },
        ].map((s, index) => (
          <div key={s.number} className="flex items-center">
            <div className="flex flex-col items-center">
              <div
                className={cn(
                  'w-10 h-10 rounded-full flex items-center justify-center font-medium mb-2',
                  step >= s.number
                    ? 'bg-primary text-primary-foreground'
                    : 'bg-muted text-muted-foreground'
                )}
              >
                {step > s.number ? <Check className="h-5 w-5" /> : s.number}
              </div>
              <span
                className={cn(
                  'text-xs font-medium whitespace-nowrap',
                  step >= s.number ? 'text-foreground' : 'text-muted-foreground'
                )}
              >
                {s.name}
              </span>
            </div>
            {index < 2 && (
              <div
                className={cn(
                  'w-24 h-1 mx-4 mb-6',
                  step > s.number ? 'bg-primary' : 'bg-muted'
                )}
              />
            )}
          </div>
        ))}
      </div>

      {/* Step 1: Basic Information */}
      {step === 1 && (
        <Card>
          <CardHeader>
            <CardTitle>Basic Information</CardTitle>
            <CardDescription>
              Give your API key a descriptive name and set an optional
              expiration date
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-6">
            <div className="space-y-2">
              <Label htmlFor="name">API Key Name *</Label>
              <Input
                id="name"
                placeholder="e.g., Production Server Key"
                value={keyName}
                onChange={(e) => setKeyName(e.target.value)}
                className="max-w-md"
              />
              <p className="text-sm text-muted-foreground">
                Choose a name that helps you remember what this key is used for
              </p>
            </div>

            <div className="space-y-2">
              <Label htmlFor="expires">
                <Calendar className="inline h-4 w-4 mr-2" />
                Expiration Date (Optional)
              </Label>
              <Input
                id="expires"
                type="date"
                value={expiresAt}
                onChange={(e) => setExpiresAt(e.target.value)}
                min={new Date().toISOString().split('T')[0]}
                className="max-w-md"
              />
              <p className="text-sm text-muted-foreground">
                Keys with expiration dates are more secure. Leave empty for no
                expiration.
              </p>
            </div>

            <div className="flex justify-end gap-3">
              <Button variant="outline" onClick={() => navigate('/keys')}>
                Cancel
              </Button>
              <Button onClick={() => setStep(2)} disabled={!canProceed()}>
                Next: Permissions
              </Button>
            </div>
          </CardContent>
        </Card>
      )}

      {/* Step 2: Permissions */}
      {step === 2 && (
        <div className="space-y-6">
          {/* Predefined Roles */}
          <Card>
            <CardHeader>
              <CardTitle>
                <Shield className="inline h-5 w-5 mr-2" />
                Choose Access Level
              </CardTitle>
              <CardDescription>
                Select a predefined role or customize permissions
              </CardDescription>
            </CardHeader>
            <CardContent>
              {isLoadingPermissions ? (
                <div className="text-center py-8">
                  Loading roles and permissions...
                </div>
              ) : (
                <div className="grid gap-4">
                  {permissionsData?.roles.map((role) => (
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
                            {role.permissions.slice(0, 5).map((p) => (
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
              )}
            </CardContent>
          </Card>

          {/* Custom Permissions */}
          <Card>
            <CardHeader>
              <CardTitle>
                <Key className="inline h-5 w-5 mr-2" />
                Custom Permissions
              </CardTitle>
              <CardDescription>
                Select specific permissions for fine-grained access control
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
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
                                    {categoryPermissions.length} permission
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
                                  openCategories.has(category) && 'rotate-90'
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
                                    checked={selectedPermissions.has(perm.name)}
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
            </CardContent>
          </Card>

          <div className="flex justify-between">
            <Button variant="outline" onClick={() => setStep(1)}>
              Back
            </Button>
            <div className="flex gap-3">
              <Button variant="outline" onClick={() => navigate('/keys')}>
                Cancel
              </Button>
              <Button onClick={() => setStep(3)} disabled={!canProceed()}>
                Next: Review
              </Button>
            </div>
          </div>
        </div>
      )}

      {/* Step 3: Review and Create */}
      {step === 3 && (
        <Card>
          <CardHeader>
            <CardTitle>Review and Create</CardTitle>
            <CardDescription>
              Review your API key configuration before creating
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-6">
            <div className="space-y-4">
              <div>
                <Label className="text-muted-foreground">Name</Label>
                <p className="font-medium">{keyName}</p>
              </div>

              <Separator />

              <div>
                <Label className="text-muted-foreground">Access Level</Label>
                <p className="font-medium">
                  {useCustomPermissions ? 'Custom Permissions' : selectedRole}
                </p>
              </div>

              <Separator />

              <div>
                <Label className="text-muted-foreground">Permissions</Label>
                <div className="flex flex-wrap gap-1 mt-2">
                  {Array.from(selectedPermissions)
                    .slice(0, 10)
                    .map((p) => (
                      <Badge key={p} variant="secondary" className="text-xs">
                        {p}
                      </Badge>
                    ))}
                  {selectedPermissions.size > 10 && (
                    <Badge variant="outline" className="text-xs">
                      +{selectedPermissions.size - 10} more
                    </Badge>
                  )}
                </div>
                <p className="text-sm text-muted-foreground mt-2">
                  Total: {selectedPermissions.size} permission
                  {selectedPermissions.size !== 1 ? 's' : ''}
                </p>
              </div>

              {expiresAt && (
                <>
                  <Separator />
                  <div>
                    <Label className="text-muted-foreground">Expires</Label>
                    <p className="font-medium">
                      {new Date(expiresAt).toLocaleDateString()}
                    </p>
                  </div>
                </>
              )}
            </div>

            <Alert>
              <AlertCircle className="h-4 w-4" />
              <AlertDescription>
                After creating this API key, you'll receive a secret token. Make
                sure to copy and store it securely as it won't be shown again.
              </AlertDescription>
            </Alert>

            <div className="flex justify-between">
              <Button variant="outline" onClick={() => setStep(2)}>
                Back
              </Button>
              <div className="flex gap-3">
                <Button variant="outline" onClick={() => navigate('/keys')}>
                  Cancel
                </Button>
                <Button
                  onClick={handleSubmit}
                  disabled={createMutation.isPending}
                >
                  {createMutation.isPending ? 'Creating...' : 'Create API Key'}
                </Button>
              </div>
            </div>
          </CardContent>
        </Card>
      )}
    </div>
  )
}
