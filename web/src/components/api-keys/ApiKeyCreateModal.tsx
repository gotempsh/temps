import { useState, useEffect } from 'react'
import { useForm } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { ScrollArea } from '@/components/ui/scroll-area'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { AlertCircle, Copy, Check, Shield, Key } from 'lucide-react'
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import type { CreateApiKeyRequest } from '@/api/client'
import { useApiKeyPermissions } from './useApiKeyPermissions'

const formSchema = z.object({
  name: z.string().min(1, 'Name is required'),
  expires_at: z
    .string()
    .optional()
    .refine(
      (val) => {
        if (!val) return true
        const expirationDate = new Date(val)
        const today = new Date()
        today.setHours(0, 0, 0, 0)
        return expirationDate > today
      },
      { message: 'Expiration date must be greater than today' }
    ),
})

type FormValues = z.infer<typeof formSchema>

interface ApiKeyCreateModalProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  onSubmit: (data: CreateApiKeyRequest) => void
  isPending: boolean
  newKeySecret: string | null
  onKeySecretDismiss: () => void
}

export function ApiKeyCreateModal({
  open,
  onOpenChange,
  onSubmit,
  isPending,
  newKeySecret,
  onKeySecretDismiss,
}: ApiKeyCreateModalProps) {
  const [copiedKey, setCopiedKey] = useState(false)
  const [selectedRole, setSelectedRole] = useState<string>('')
  const [selectedPermissions, setSelectedPermissions] = useState<Set<string>>(
    new Set()
  )
  const [useCustomPermissions, setUseCustomPermissions] = useState(false)

  const { data: permissionsData, isLoading: isLoadingPermissions } =
    useApiKeyPermissions()

  const form = useForm<FormValues>({
    resolver: zodResolver(formSchema),
    defaultValues: {
      name: '',
      expires_at: '',
    },
  })

  const handleCopyKey = async () => {
    if (newKeySecret) {
      await navigator.clipboard.writeText(newKeySecret)
      setCopiedKey(true)
      setTimeout(() => setCopiedKey(false), 2000)
    }
  }

  const handleFormSubmit = (values: FormValues) => {
    const data: CreateApiKeyRequest = {
      name: values.name,
      role_type: useCustomPermissions ? 'custom' : selectedRole,
      permissions: useCustomPermissions
        ? Array.from(selectedPermissions)
        : undefined,
      expires_at: values.expires_at
        ? new Date(values.expires_at).toISOString()
        : undefined,
    }
    onSubmit(data)
  }

  const handleClose = () => {
    if (newKeySecret) {
      onKeySecretDismiss()
    }
    form.reset()
    setSelectedRole('')
    setSelectedPermissions(new Set())
    setUseCustomPermissions(false)
    onOpenChange(false)
  }

  // Reset form when dialog opens
  useEffect(() => {
    if (open && !newKeySecret) {
      form.reset()
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setSelectedRole('')
      setSelectedPermissions(new Set())
      setUseCustomPermissions(false)
    }
  }, [open, newKeySecret, form])

  const handleRoleChange = (role: string) => {
    setSelectedRole(role)
    setUseCustomPermissions(false)
    // Auto-select permissions based on role
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

  return (
    <Dialog open={open} onOpenChange={handleClose}>
      <DialogContent className="sm:max-w-[600px]">
        {newKeySecret ? (
          <>
            <DialogHeader>
              <DialogTitle>API Key Created Successfully</DialogTitle>
              <DialogDescription>
                Copy your API key now. You won&apos;t be able to see it again!
              </DialogDescription>
            </DialogHeader>
            <div className="space-y-4 py-4">
              <Alert>
                <AlertCircle className="h-4 w-4" />
                <AlertDescription>
                  Make sure to copy your API key now. For security reasons, we
                  won&apos;t show it again.
                </AlertDescription>
              </Alert>
              <div className="space-y-2">
                <FormLabel>Your API Key</FormLabel>
                <div className="flex gap-2">
                  <Input
                    value={newKeySecret}
                    readOnly
                    className="font-mono text-sm"
                  />
                  <Button size="sm" variant="outline" onClick={handleCopyKey}>
                    {copiedKey ? (
                      <Check className="h-4 w-4" />
                    ) : (
                      <Copy className="h-4 w-4" />
                    )}
                  </Button>
                </div>
              </div>
            </div>
            <DialogFooter>
              <Button onClick={handleClose}>Done</Button>
            </DialogFooter>
          </>
        ) : (
          <>
            <DialogHeader>
              <DialogTitle>Create API Key</DialogTitle>
              <DialogDescription>
                Create a new API key with specific roles and permissions.
              </DialogDescription>
            </DialogHeader>
            <Form {...form}>
              <form onSubmit={form.handleSubmit(handleFormSubmit)}>
                <div className="space-y-4 py-4">
                  <FormField
                    control={form.control}
                    name="name"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Name</FormLabel>
                        <FormControl>
                          <Input placeholder="Production API Key" {...field} />
                        </FormControl>
                        <FormMessage />
                      </FormItem>
                    )}
                  />

                  <Tabs
                    defaultValue="role"
                    onValueChange={(v) =>
                      setUseCustomPermissions(v === 'custom')
                    }
                  >
                    <TabsList className="grid w-full grid-cols-2">
                      <TabsTrigger value="role">
                        <Shield className="h-4 w-4 mr-2" />
                        Predefined Role
                      </TabsTrigger>
                      <TabsTrigger value="custom">
                        <Key className="h-4 w-4 mr-2" />
                        Custom Permissions
                      </TabsTrigger>
                    </TabsList>

                    <TabsContent value="role" className="space-y-4">
                      <div className="space-y-2">
                        <FormLabel>Select Role</FormLabel>
                        {isLoadingPermissions ? (
                          <div className="text-sm text-muted-foreground">
                            Loading roles...
                          </div>
                        ) : (
                          <Select
                            value={selectedRole}
                            onValueChange={handleRoleChange}
                          >
                            <SelectTrigger>
                              <SelectValue placeholder="Select a role" />
                            </SelectTrigger>
                            <SelectContent>
                              {permissionsData?.roles.map((role) => (
                                <SelectItem key={role.name} value={role.name}>
                                  <div>
                                    <div className="font-medium">
                                      {role.name}
                                    </div>
                                    <div className="text-xs text-muted-foreground">
                                      {role.description}
                                    </div>
                                  </div>
                                </SelectItem>
                              ))}
                            </SelectContent>
                          </Select>
                        )}
                        {selectedRole && permissionsData && (
                          <div className="mt-2 p-3 bg-muted rounded-md">
                            <p className="text-sm font-medium mb-2">
                              Permissions included:
                            </p>
                            <div className="text-xs text-muted-foreground space-y-1">
                              {permissionsData.roles
                                .find((r) => r.name === selectedRole)
                                ?.permissions.map((p) => (
                                  <div key={p}>• {p}</div>
                                ))}
                            </div>
                          </div>
                        )}
                      </div>
                    </TabsContent>

                    <TabsContent value="custom" className="space-y-4">
                      <div className="space-y-2">
                        <FormLabel>Select Permissions</FormLabel>
                        {isLoadingPermissions ? (
                          <div className="text-sm text-muted-foreground">
                            Loading permissions...
                          </div>
                        ) : (
                          <ScrollArea className="h-[300px] border rounded-md p-3">
                            {permissionsByCategory &&
                              Object.entries(permissionsByCategory).map(
                                ([category, perms]) => (
                                  <div key={category} className="mb-4">
                                    <h4 className="font-medium text-sm mb-2">
                                      {category}
                                    </h4>
                                    <div className="space-y-2 ml-2">
                                      {perms.map((perm) => (
                                        <div
                                          key={perm.name}
                                          className="flex items-start space-x-2"
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
                                          <div className="grid gap-1.5 leading-none">
                                            <label
                                              htmlFor={perm.name}
                                              className="text-sm font-medium leading-none peer-disabled:cursor-not-allowed peer-disabled:opacity-70"
                                            >
                                              {perm.name}
                                            </label>
                                            <p className="text-xs text-muted-foreground">
                                              {perm.description}
                                            </p>
                                          </div>
                                        </div>
                                      ))}
                                    </div>
                                  </div>
                                )
                              )}
                          </ScrollArea>
                        )}
                        {selectedPermissions.size > 0 && (
                          <div className="text-xs text-muted-foreground">
                            {selectedPermissions.size} permission
                            {selectedPermissions.size !== 1 ? 's' : ''} selected
                          </div>
                        )}
                      </div>
                    </TabsContent>
                  </Tabs>

                  <FormField
                    control={form.control}
                    name="expires_at"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Expiration Date (optional)</FormLabel>
                        <FormControl>
                          <Input
                            type="date"
                            min={new Date().toISOString().split('T')[0]}
                            {...field}
                          />
                        </FormControl>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                </div>
                <DialogFooter>
                  <Button
                    type="button"
                    variant="outline"
                    onClick={() => onOpenChange(false)}
                  >
                    Cancel
                  </Button>
                  <Button
                    type="submit"
                    disabled={
                      isPending ||
                      (!useCustomPermissions && !selectedRole) ||
                      (useCustomPermissions && selectedPermissions.size === 0)
                    }
                  >
                    {isPending ? 'Creating...' : 'Create'}
                  </Button>
                </DialogFooter>
              </form>
            </Form>
          </>
        )}
      </DialogContent>
    </Dialog>
  )
}
