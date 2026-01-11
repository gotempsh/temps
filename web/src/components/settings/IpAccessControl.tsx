import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Textarea } from '@/components/ui/textarea'
import {
  createIpAccessControlMutation,
  deleteIpAccessControlMutation,
  listIpAccessControlOptions,
  updateIpAccessControlMutation,
  type IpAccessControlResponse,
} from '@/api/client/@tanstack/react-query.gen'
import { Ban, Plus, Shield, Trash2, Edit, Loader2 } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useForm } from 'react-hook-form'
import { useMutation, useQuery } from '@tanstack/react-query'
import { toast } from 'sonner'
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'

const ipAccessControlSchema = z.object({
  ip_address: z
    .string()
    .min(1, 'IP address is required')
    .refine(
      (val) => {
        // Basic IPv4 validation (including CIDR)
        const ipv4Regex = /^(\d{1,3}\.){3}\d{1,3}(\/\d{1,2})?$/
        return ipv4Regex.test(val)
      },
      {
        message:
          'Invalid IP address format. Use format: 192.168.1.1 or 10.0.0.0/24',
      }
    ),
  action: z.enum(['block', 'allow'], {
    message: 'Action is required',
  }),
  reason: z.string().optional(),
})

type IpAccessControlFormData = z.infer<typeof ipAccessControlSchema>

export function IpAccessControl() {
  const [isCreateDialogOpen, setIsCreateDialogOpen] = useState(false)
  const [isEditDialogOpen, setIsEditDialogOpen] = useState(false)
  const [editingRule, setEditingRule] =
    useState<IpAccessControlResponse | null>(null)

  const {
    data: rules = [],
    isLoading,
    refetch,
  } = useQuery(listIpAccessControlOptions())

  const createMutation = useMutation({
    ...createIpAccessControlMutation(),
    onSuccess: async () => {
      await refetch()
      toast.success('IP access control rule created successfully')
      setIsCreateDialogOpen(false)
    },
    onError: (error: any) => {
      toast.error(error?.message || 'Failed to create IP access control rule')
    },
  })

  const updateMutation = useMutation({
    ...updateIpAccessControlMutation(),
    onSuccess: async () => {
      await refetch()
      toast.success('IP access control rule updated successfully')
      setIsEditDialogOpen(false)
      setEditingRule(null)
    },
    onError: (error: any) => {
      toast.error(error?.message || 'Failed to update IP access control rule')
    },
  })

  const deleteMutation = useMutation({
    ...deleteIpAccessControlMutation(),
    onSuccess: async () => {
      await refetch()
      toast.success('IP access control rule deleted successfully')
    },
    onError: (error: any) => {
      toast.error(error?.message || 'Failed to delete IP access control rule')
    },
  })

  const {
    register: registerCreate,
    handleSubmit: handleSubmitCreate,
    formState: { errors: createErrors },
    reset: resetCreate,
    setValue: setCreateValue,
  } = useForm<IpAccessControlFormData>({
    resolver: zodResolver(ipAccessControlSchema),
    defaultValues: {
      action: 'block',
    },
  })

  const {
    register: registerEdit,
    handleSubmit: handleSubmitEdit,
    formState: { errors: editErrors },
    reset: resetEdit,
    setValue: setEditValue,
  } = useForm<IpAccessControlFormData>({
    resolver: zodResolver(ipAccessControlSchema),
  })

  const onCreateSubmit = (data: IpAccessControlFormData) => {
    createMutation.mutate({
      body: {
        ip_address: data.ip_address,
        action: data.action,
        reason: data.reason || null,
      },
    })
  }

  const onEditSubmit = (data: IpAccessControlFormData) => {
    if (!editingRule) return

    updateMutation.mutate({
      path: { id: editingRule.id },
      body: {
        ip_address:
          data.ip_address !== editingRule.ip_address
            ? data.ip_address
            : undefined,
        action: data.action !== editingRule.action ? data.action : undefined,
        reason:
          data.reason !== editingRule.reason ? data.reason || null : undefined,
      },
    })
  }

  const handleEdit = (rule: IpAccessControlResponse) => {
    setEditingRule(rule)
    resetEdit({
      ip_address: rule.ip_address,
      action: rule.action as 'block' | 'allow',
      reason: rule.reason || '',
    })
    setIsEditDialogOpen(true)
  }

  const handleDelete = (id: number) => {
    if (
      confirm('Are you sure you want to delete this IP access control rule?')
    ) {
      deleteMutation.mutate({ path: { id } })
    }
  }

  const handleCreateDialogOpen = (open: boolean) => {
    setIsCreateDialogOpen(open)
    if (!open) {
      resetCreate({ ip_address: '', action: 'block', reason: '' })
    }
  }

  const handleEditDialogOpen = (open: boolean) => {
    setIsEditDialogOpen(open)
    if (!open) {
      setEditingRule(null)
    }
  }

  const blockedRules = useMemo(
    () => rules?.filter((r) => r.action === 'block'),
    [rules]
  )
  const allowedRules = useMemo(
    () => rules?.filter((r) => r.action === 'allow'),
    [rules]
  )

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Shield className="h-5 w-5" />
          IP Access Control
        </CardTitle>
        <CardDescription>
          Block or allow specific IPs from accessing Temps at the infrastructure
          level (Pingora proxy)
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-6">
        <div className="flex justify-end">
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => handleCreateDialogOpen(true)}
          >
            <Plus className="h-4 w-4 mr-2" />
            Add IP Rule
          </Button>
        </div>

        {isLoading ? (
          <div className="flex items-center justify-center py-8">
            <Loader2 className="h-6 w-6 animate-spin" />
          </div>
        ) : (
          <div className="space-y-6">
            {/* Blocked IPs Section */}
            <div>
              <div className="flex items-center gap-2 mb-3">
                <Ban className="h-4 w-4 text-destructive" />
                <Label className="text-base font-semibold">
                  Blocked IPs ({blockedRules.length})
                </Label>
              </div>
              <p className="text-sm text-muted-foreground mb-3">
                IPs that are completely blocked from accessing Temps
              </p>
              <div className="space-y-2">
                {blockedRules.length === 0 ? (
                  <div className="text-sm text-muted-foreground py-4 text-center border border-dashed rounded-md">
                    No blocked IPs configured
                  </div>
                ) : (
                  blockedRules.map((rule) => (
                    <div
                      key={rule.id}
                      className="flex items-center justify-between p-3 border rounded-md"
                    >
                      <div className="flex-1">
                        <div className="font-mono text-sm font-medium">
                          {rule.ip_address}
                        </div>
                        {rule.reason && (
                          <div className="text-xs text-muted-foreground mt-1">
                            {rule.reason}
                          </div>
                        )}
                      </div>
                      <div className="flex gap-2">
                        <Button
                          type="button"
                          variant="ghost"
                          size="icon"
                          onClick={() => handleEdit(rule)}
                        >
                          <Edit className="h-4 w-4" />
                        </Button>
                        <Button
                          type="button"
                          variant="ghost"
                          size="icon"
                          onClick={() => handleDelete(rule.id)}
                          disabled={deleteMutation.isPending}
                        >
                          <Trash2 className="h-4 w-4" />
                        </Button>
                      </div>
                    </div>
                  ))
                )}
              </div>
            </div>

            {/* Allowed IPs Section */}
            <div>
              <div className="flex items-center gap-2 mb-3">
                <Shield className="h-4 w-4 text-green-600" />
                <Label className="text-base font-semibold">
                  Allowed IPs ({allowedRules.length})
                </Label>
              </div>
              <p className="text-sm text-muted-foreground mb-3">
                IPs that are explicitly allowed (useful with default deny
                policies)
              </p>
              <div className="space-y-2">
                {allowedRules.length === 0 ? (
                  <div className="text-sm text-muted-foreground py-4 text-center border border-dashed rounded-md">
                    No allowed IPs configured
                  </div>
                ) : (
                  allowedRules.map((rule) => (
                    <div
                      key={rule.id}
                      className="flex items-center justify-between p-3 border rounded-md"
                    >
                      <div className="flex-1">
                        <div className="font-mono text-sm font-medium">
                          {rule.ip_address}
                        </div>
                        {rule.reason && (
                          <div className="text-xs text-muted-foreground mt-1">
                            {rule.reason}
                          </div>
                        )}
                      </div>
                      <div className="flex gap-2">
                        <Button
                          type="button"
                          variant="ghost"
                          size="icon"
                          onClick={() => handleEdit(rule)}
                        >
                          <Edit className="h-4 w-4" />
                        </Button>
                        <Button
                          type="button"
                          variant="ghost"
                          size="icon"
                          onClick={() => handleDelete(rule.id)}
                          disabled={deleteMutation.isPending}
                        >
                          <Trash2 className="h-4 w-4" />
                        </Button>
                      </div>
                    </div>
                  ))
                )}
              </div>
            </div>
          </div>
        )}

        {/* Create Dialog */}
        <Dialog open={isCreateDialogOpen} onOpenChange={handleCreateDialogOpen}>
          <DialogContent>
            <form
              onSubmit={(e) => {
                e.preventDefault()
                e.stopPropagation()
                handleSubmitCreate(onCreateSubmit)(e)
              }}
            >
              <DialogHeader>
                <DialogTitle>Add IP Access Control Rule</DialogTitle>
                <DialogDescription>
                  Block or allow specific IPs from accessing your platform
                </DialogDescription>
              </DialogHeader>
              <div className="space-y-4 py-4">
                <div className="space-y-2">
                  <Label htmlFor="create-ip">IP Address</Label>
                  <Input
                    id="create-ip"
                    placeholder="192.168.1.1 or 10.0.0.0/24"
                    {...registerCreate('ip_address')}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter') {
                        e.preventDefault()
                        e.stopPropagation()
                        handleSubmitCreate(onCreateSubmit)(e)
                      }
                    }}
                  />
                  {createErrors.ip_address && (
                    <p className="text-sm text-destructive">
                      {createErrors.ip_address.message}
                    </p>
                  )}
                  <p className="text-xs text-muted-foreground">
                    Supports IPv4 addresses and CIDR notation
                  </p>
                </div>

                <div className="space-y-2">
                  <Label htmlFor="create-action">Action</Label>
                  <Select
                    defaultValue="block"
                    onValueChange={(value) =>
                      setCreateValue('action', value as 'block' | 'allow')
                    }
                  >
                    <SelectTrigger id="create-action">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="block">Block</SelectItem>
                      <SelectItem value="allow">Allow</SelectItem>
                    </SelectContent>
                  </Select>
                </div>

                <div className="space-y-2">
                  <Label htmlFor="create-reason">Reason (Optional)</Label>
                  <Textarea
                    id="create-reason"
                    placeholder="Why is this IP being blocked/allowed?"
                    {...registerCreate('reason')}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
                        e.preventDefault()
                        e.stopPropagation()
                        handleSubmitCreate(onCreateSubmit)(e)
                      }
                    }}
                  />
                </div>
              </div>
              <DialogFooter>
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => handleCreateDialogOpen(false)}
                >
                  Cancel
                </Button>
                <Button type="submit" disabled={createMutation.isPending}>
                  {createMutation.isPending ? (
                    <>
                      <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                      Creating...
                    </>
                  ) : (
                    'Create Rule'
                  )}
                </Button>
              </DialogFooter>
            </form>
          </DialogContent>
        </Dialog>

        {/* Edit Dialog */}
        <Dialog open={isEditDialogOpen} onOpenChange={handleEditDialogOpen}>
          <DialogContent>
            <form
              onSubmit={(e) => {
                e.preventDefault()
                e.stopPropagation()
                handleSubmitEdit(onEditSubmit)(e)
              }}
            >
              <DialogHeader>
                <DialogTitle>Edit IP Access Control Rule</DialogTitle>
                <DialogDescription>
                  Update the IP address, action, or reason
                </DialogDescription>
              </DialogHeader>
              <div className="space-y-4 py-4">
                <div className="space-y-2">
                  <Label htmlFor="edit-ip">IP Address</Label>
                  <Input
                    id="edit-ip"
                    placeholder="192.168.1.1 or 10.0.0.0/24"
                    {...registerEdit('ip_address')}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter') {
                        e.preventDefault()
                        e.stopPropagation()
                        handleSubmitEdit(onEditSubmit)(e)
                      }
                    }}
                  />
                  {editErrors.ip_address && (
                    <p className="text-sm text-destructive">
                      {editErrors.ip_address.message}
                    </p>
                  )}
                </div>

                <div className="space-y-2">
                  <Label htmlFor="edit-action">Action</Label>
                  <Select
                    value={editingRule?.action}
                    onValueChange={(value) =>
                      setEditValue('action', value as 'block' | 'allow')
                    }
                  >
                    <SelectTrigger id="edit-action">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="block">Block</SelectItem>
                      <SelectItem value="allow">Allow</SelectItem>
                    </SelectContent>
                  </Select>
                </div>

                <div className="space-y-2">
                  <Label htmlFor="edit-reason">Reason (Optional)</Label>
                  <Textarea
                    id="edit-reason"
                    placeholder="Why is this IP being blocked/allowed?"
                    {...registerEdit('reason')}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
                        e.preventDefault()
                        e.stopPropagation()
                        handleSubmitEdit(onEditSubmit)(e)
                      }
                    }}
                  />
                </div>
              </div>
              <DialogFooter>
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => handleEditDialogOpen(false)}
                >
                  Cancel
                </Button>
                <Button type="submit" disabled={updateMutation.isPending}>
                  {updateMutation.isPending ? (
                    <>
                      <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                      Updating...
                    </>
                  ) : (
                    'Update Rule'
                  )}
                </Button>
              </DialogFooter>
            </form>
          </DialogContent>
        </Dialog>
      </CardContent>
    </Card>
  )
}
