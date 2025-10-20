'use client'

import {
  deleteProviderMutation,
  listNotificationProvidersOptions,
  testProviderMutation,
  updateEmailProviderMutation,
  updateProviderMutation,
  updateSlackProviderMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { NotificationProviderResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { EmptyState } from '@/components/ui/empty-state'
import { Switch } from '@/components/ui/switch'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery } from '@tanstack/react-query'
import { Bell, EllipsisVertical, Plus } from 'lucide-react'
import { useNavigate } from 'react-router-dom'
import { useMemo, useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'
import { ProviderForm } from './ProviderForm'
import { ProviderFormData, providerSchema } from './schemas'

interface ExtendedNotificationProvider extends NotificationProviderResponse {
  provider_type: 'email' | 'slack'
  config: {
    // Slack config
    webhook_url?: string
    channel?: string
    slack_username?: string

    // Email config
    smtp_host?: string
    smtp_port?: number
    username?: string
    password?: string
    from_address?: string
    from_name?: string
    to_addresses?: string[]
  }
}

export function ProvidersManagement() {
  const navigate = useNavigate()
  const [editingProvider, setEditingProvider] =
    useState<ExtendedNotificationProvider | null>(null)
  const [isEditDialogOpen, setIsEditDialogOpen] = useState(false)

  const {
    data: providers,
    isLoading,
    refetch,
  } = useQuery({
    ...listNotificationProvidersOptions(),
  })

  const updateEmailMutation = useMutation({
    ...updateEmailProviderMutation(),
    meta: {
      errorTitle: 'Failed to update email provider',
    },
    onSuccess: () => {
      toast.success('Email provider updated successfully')
      setIsEditDialogOpen(false)
      setEditingProvider(null)
      refetch()
    },
  })

  const updateSlackMutation = useMutation({
    ...updateSlackProviderMutation(),
    meta: {
      errorTitle: 'Failed to update Slack provider',
    },
    onSuccess: () => {
      toast.success('Slack provider updated successfully')
      setIsEditDialogOpen(false)
      setEditingProvider(null)
      refetch()
    },
  })

  const toggleEnabledMutation = useMutation({
    ...updateProviderMutation(),
    meta: {
      errorTitle: 'Failed to update provider status',
    },
    onSuccess: () => {
      toast.success('Provider status updated successfully')
      refetch()
    },
  })

  const deleteMutation = useMutation({
    ...deleteProviderMutation(),
    meta: {
      errorTitle: 'Failed to delete provider',
    },
    onSuccess: () => {
      toast.success('Provider deleted successfully')
      refetch()
    },
  })

  const testMutation = useMutation({
    ...testProviderMutation(),
    meta: {
      errorTitle: 'Failed to test provider',
    },
  })

  const editForm = useForm<ProviderFormData>({
    resolver: zodResolver(providerSchema),
    defaultValues: {
      name: '',
      provider_type: 'email',
      config: {},
    },
  })

  const onEditSubmit = async (data: ProviderFormData) => {
    if (!editingProvider) return

    if (data.provider_type === 'slack') {
      await updateSlackMutation.mutateAsync({
        path: { id: editingProvider.id },
        body: {
          name: data.name,
          enabled: editingProvider.enabled,
          config: {
            webhook_url: data.config.webhook_url!,
            channel: data.config.channel ?? null,
          },
        },
      })
    } else {
      await updateEmailMutation.mutateAsync({
        path: { id: editingProvider.id },
        body: {
          name: data.name,
          enabled: editingProvider.enabled,
          config: {
            smtp_host: data.config.smtp_host!,
            smtp_port: data.config.smtp_port!,
            username: data.config.use_credentials
              ? data.config.smtp_username || ''
              : '',
            password: data.config.use_credentials
              ? data.config.password || ''
              : '',
            from_address: data.config.from_address!,
            to_addresses: data.config.to_addresses!,
            from_name: data.config.from_name || undefined,
            tls_mode: data.config.tls_mode || undefined,
            starttls_required: data.config.starttls_required,
            accept_invalid_certs: data.config.accept_invalid_certs,
          },
        },
      })
    }
  }

  const handleDelete = async (provider: ExtendedNotificationProvider) => {
    await deleteMutation.mutateAsync({
      path: { provider_id: provider.id },
    })
  }

  const handleTest = async (provider: ExtendedNotificationProvider) => {
    toast.promise(testMutation.mutateAsync({ path: { id: provider.id } }), {
      loading: 'Sending test notification...',
      success: (data) => data.message || 'Test notification sent successfully!',
      error: (error) => {
        const message =
          error?.response?.data?.detail || 'Failed to send test notification'
        return message
      },
    })
  }

  const handleEdit = (provider: ExtendedNotificationProvider) => {
    navigate(`/monitoring/providers/edit/${provider.id}`)
  }

  const handleToggleEnabled = async (
    provider: ExtendedNotificationProvider
  ) => {
    await toggleEnabledMutation.mutateAsync({
      path: { id: provider.id },
      body: {
        name: provider.name,
        enabled: !provider.enabled,
        config: provider.config,
      },
    })
  }

  const hasProviders = useMemo(
    () => providers && providers.length > 0,
    [providers]
  )
  const watchedProviderType = useWatch({
    control: editForm.control,
    name: 'provider_type',
  })
  const isLoadingProviderType = useMemo(
    () =>
      watchedProviderType === 'email'
        ? updateEmailMutation.isPending
        : updateSlackMutation.isPending,
    [
      watchedProviderType,
      updateEmailMutation.isPending,
      updateSlackMutation.isPending,
    ]
  )
  return (
    <div className="space-y-4">
      <div className="flex justify-between items-center">
        <div>
          <h2 className="text-2xl font-bold tracking-tight">
            Notification Providers
          </h2>
          <p className="text-muted-foreground">
            Manage your notification providers for alerts and updates.
          </p>
        </div>

        {hasProviders && (
          <Button onClick={() => navigate('/monitoring/providers/add')}>
            <Plus className="h-4 w-4 mr-2" />
            Add Provider
          </Button>
        )}
      </div>

      {!hasProviders && !isLoading ? (
        <EmptyState
          icon={Bell}
          title="No notification providers configured"
          description="Add your first notification provider to start receiving alerts about your deployments and infrastructure."
          action={
            <Button onClick={() => navigate('/monitoring/providers/add')}>
              <Plus className="h-4 w-4 mr-2" />
              Add Provider
            </Button>
          }
        />
      ) : (
        <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
          {providers?.map((provider) => {
            const typedProvider = provider as ExtendedNotificationProvider
            return (
              <Card key={provider.id}>
                <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
                  <div className="space-y-1">
                    <CardTitle className="text-base font-medium leading-none">
                      {provider.name}
                    </CardTitle>
                    <p className="text-xs text-muted-foreground capitalize">
                      {provider.provider_type}
                    </p>
                  </div>
                  <div className="flex items-center gap-1">
                    <Switch
                      checked={provider.enabled}
                      onCheckedChange={() => handleToggleEnabled(typedProvider)}
                      disabled={toggleEnabledMutation.isPending}
                      className="data-[state=checked]:bg-primary"
                    />
                    <DropdownMenu>
                      <DropdownMenuTrigger asChild>
                        <Button variant="ghost" size="icon" className="h-8 w-8">
                          <EllipsisVertical className="h-4 w-4" />
                        </Button>
                      </DropdownMenuTrigger>
                      <DropdownMenuContent align="end">
                        <DropdownMenuItem
                          onClick={() => handleEdit(typedProvider)}
                        >
                          Edit
                        </DropdownMenuItem>
                        <DropdownMenuItem
                          onClick={() => handleTest(typedProvider)}
                        >
                          Test
                        </DropdownMenuItem>
                        <DropdownMenuSeparator />
                        <DropdownMenuItem
                          className="text-destructive"
                          onClick={() => handleDelete(typedProvider)}
                        >
                          Delete
                        </DropdownMenuItem>
                      </DropdownMenuContent>
                    </DropdownMenu>
                  </div>
                </CardHeader>
                <CardContent>
                  <p className="text-sm text-muted-foreground truncate">
                    {provider.provider_type === 'email'
                      ? typedProvider.config.from_address
                      : typedProvider.config.webhook_url}
                  </p>
                </CardContent>
              </Card>
            )
          })}
        </div>
      )}

      <Dialog open={isEditDialogOpen} onOpenChange={setIsEditDialogOpen}>
        <DialogContent className="max-w-2xl max-h-[90vh] flex flex-col">
          <DialogHeader>
            <DialogTitle>Edit Provider</DialogTitle>
            <DialogDescription>
              Update your notification provider settings.
            </DialogDescription>
          </DialogHeader>
          <div className="flex-1 overflow-y-auto">
            <ProviderForm
              form={editForm}
              onSubmit={onEditSubmit}
              isEdit
              isLoading={isLoadingProviderType}
            />
          </div>
        </DialogContent>
      </Dialog>
    </div>
  )
}
