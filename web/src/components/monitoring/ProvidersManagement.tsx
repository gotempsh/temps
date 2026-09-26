// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

'use client'

import {
  deleteNotificationProviderMutation as deleteProviderSafelyMutation,
  listNotificationProvidersOptions,
  testNotificationProviderMutation as testProviderMutation,
  updateNotificationEmailProviderMutation,
  updateNotificationProviderMutation,
  updateSlackProviderMutation,
  updateWebhookProviderMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { revealNotificationProviderConfig } from '@/api/client/sdk.gen'
import { NotificationProviderResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import { CreateActionButton } from '@/components/ui/create-action-button'
import { fmtDateTime, fmtRelativeTime } from '@temps-sdk/ds'
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
import { Bell, EllipsisVertical } from 'lucide-react'
import { useNavigate } from 'react-router'
import { useMemo, useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { toast } from 'sonner'
import { ProviderForm } from './ProviderForm'
import { NotificationProviderIcon } from './NotificationProviderIcon'
import { ProviderFormData, providerUpdateSchema } from './schemas'

interface ExtendedNotificationProvider extends NotificationProviderResponse {
  provider_type: 'email' | 'slack' | 'webhook' | 'cloudflare'
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

    // Webhook config
    url?: string
    method?: 'POST' | 'PUT' | 'PATCH'
    headers?: Record<string, string>
    timeout_secs?: number

    // Cloudflare config
    account_id?: string
    api_token?: string
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
    ...updateNotificationEmailProviderMutation(),
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

  const updateWebhookMutation = useMutation({
    ...updateWebhookProviderMutation(),
    meta: {
      errorTitle: 'Failed to update Webhook provider',
    },
    onSuccess: () => {
      toast.success('Webhook provider updated successfully')
      setIsEditDialogOpen(false)
      setEditingProvider(null)
      refetch()
    },
  })

  // Cloudflare uses the generic notification-provider endpoint (provider_type
  // + opaque config) rather than a dedicated typed mutation.
  const updateCloudflareMutation = useMutation({
    ...updateNotificationProviderMutation(),
    meta: {
      errorTitle: 'Failed to update Cloudflare provider',
    },
    onSuccess: () => {
      toast.success('Cloudflare provider updated successfully')
      setIsEditDialogOpen(false)
      setEditingProvider(null)
      refetch()
    },
  })

  const toggleEnabledMutation = useMutation({
    ...updateNotificationProviderMutation(),
    meta: {
      errorTitle: 'Failed to update provider status',
    },
    onSuccess: () => {
      toast.success('Provider status updated successfully')
      refetch()
    },
  })

  const deleteMutation = useMutation({
    ...deleteProviderSafelyMutation(),
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
    resolver: zodResolver(providerUpdateSchema),
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
    } else if (data.provider_type === 'webhook') {
      await updateWebhookMutation.mutateAsync({
        path: { id: editingProvider.id },
        body: {
          name: data.name,
          enabled: editingProvider.enabled,
          config: {
            url: data.config.url!,
            method: data.config.method || 'POST',
            headers: (data.config.headers || {}) as Record<string, string>,
            timeout_secs: data.config.timeout_secs || 30,
          },
        },
      })
    } else if (data.provider_type === 'cloudflare') {
      await updateCloudflareMutation.mutateAsync({
        path: { id: editingProvider.id },
        body: {
          name: data.name,
          enabled: editingProvider.enabled,
          config: {
            account_id: data.config.account_id!,
            api_token: data.config.api_token!,
            from_address: data.config.from_address!,
            from_name: data.config.from_name || undefined,
            to_addresses: data.config.to_addresses!,
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

  const handleRevealCredential = async (field: string) => {
    if (!editingProvider) {
      throw new Error('No notification provider is selected')
    }

    const { data } = await revealNotificationProviderConfig({
      path: {
        id: editingProvider.id,
        field,
      },
      throwOnError: true,
    })
    return data.value
  }

  const handleDelete = async (provider: ExtendedNotificationProvider) => {
    await deleteMutation.mutateAsync({
      path: { id: provider.id },
    })
  }

  const handleTest = async (provider: ExtendedNotificationProvider) => {
    toast.promise(
      testMutation.mutateAsync({
        path: { id: provider.id },
      }),
      {
        loading: 'Sending test notification...',
        success: (data) =>
          data.success
            ? 'Test notification sent successfully!'
            : data.message || 'Failed to send test notification',
        error: (error) => {
          const message =
            error?.response?.data?.detail || 'Failed to send test notification'
          return message
        },
      }
    )
  }

  const handleEdit = (provider: ExtendedNotificationProvider) => {
    navigate(`/settings/notifications/${provider.id}`)
  }

  const handleToggleEnabled = async (
    provider: ExtendedNotificationProvider
  ) => {
    await toggleEnabledMutation.mutateAsync({
      path: { id: provider.id },
      body: {
        name: provider.name,
        enabled: !provider.enabled,
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
        : watchedProviderType === 'slack'
          ? updateSlackMutation.isPending
          : watchedProviderType === 'cloudflare'
            ? updateCloudflareMutation.isPending
            : updateWebhookMutation.isPending,
    [
      watchedProviderType,
      updateEmailMutation.isPending,
      updateSlackMutation.isPending,
      updateCloudflareMutation.isPending,
      updateWebhookMutation.isPending,
    ]
  )
  return (
    <div className="space-y-4">
      <div className="flex justify-end">
        {/* Only one of this and the empty-state button is ever mounted, so
            the `N` shortcut is registered exactly once either way. */}
        {hasProviders && (
          <CreateActionButton
            onClick={() => navigate('/settings/notifications/new')}
            label="Add Provider"
          />
        )}
      </div>

      {!hasProviders && !isLoading ? (
        <EmptyState
          icon={Bell}
          title="No notification providers configured"
          description="Add your first notification provider to start receiving alerts about your deployments and infrastructure."
          action={
            <CreateActionButton
              onClick={() => navigate('/settings/notifications/new')}
              label="Add Provider"
            />
          }
        />
      ) : (
        <ul
          aria-label="Notification providers"
          className="divide-y rounded-lg border"
        >
          {providers?.map((provider) => {
            const typedProvider = provider as ExtendedNotificationProvider
            const config = typedProvider.config
            const destination =
              provider.provider_type === 'email' ||
              provider.provider_type === 'cloudflare'
                ? config?.from_address
                  ? `From ${config.from_address}`
                  : 'No sender address configured'
                : provider.provider_type === 'slack'
                  ? config?.channel && config.channel !== '***'
                    ? config.channel
                    : config?.webhook_url
                      ? 'Webhook configured'
                      : 'No webhook configured'
                  : config?.url
                    ? 'Webhook configured'
                    : 'No webhook configured'
            return (
              <li
                key={provider.id}
                className="flex flex-wrap items-start gap-4 p-4 sm:items-center sm:p-5"
              >
                <span className="flex size-9 shrink-0 items-center justify-center rounded-md bg-muted">
                  <NotificationProviderIcon provider={provider.provider_type} />
                </span>
                <div className="min-w-0 flex-1 basis-48 space-y-1">
                  <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
                    <h3 className="text-sm font-medium">{provider.name}</h3>
                    <span className="text-xs text-muted-foreground capitalize">
                      {provider.provider_type}
                    </span>
                  </div>
                  <p className="break-words text-sm text-muted-foreground">
                    {destination}
                  </p>
                  <p className="text-xs text-muted-foreground">
                    {Array.isArray(config?.to_addresses) &&
                      config.to_addresses.length > 0 && (
                        <span>
                          {config.to_addresses.length}{' '}
                          {config.to_addresses.length === 1
                            ? 'recipient'
                            : 'recipients'}{' '}
                          ·{' '}
                        </span>
                      )}
                    Updated{' '}
                    <time
                      dateTime={new Date(provider.updated_at).toISOString()}
                      title={fmtDateTime(provider.updated_at)}
                    >
                      {fmtRelativeTime(provider.updated_at)}
                    </time>
                  </p>
                </div>
                <div className="flex shrink-0 items-center gap-3">
                  <span className="text-xs text-muted-foreground">
                    {provider.enabled ? 'Enabled' : 'Disabled'}
                  </span>
                  <Switch
                    aria-label={`Enable ${provider.name}`}
                    checked={provider.enabled}
                    onCheckedChange={() => handleToggleEnabled(typedProvider)}
                    disabled={toggleEnabledMutation.isPending}
                    className="data-[state=checked]:bg-primary"
                  />
                  <DropdownMenu>
                    <DropdownMenuTrigger asChild>
                      <Button
                        variant="ghost"
                        size="icon"
                        className="h-8 w-8"
                        aria-label={`Actions for ${provider.name}`}
                      >
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
              </li>
            )
          })}
        </ul>
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
              revealScopeKey={`${editingProvider?.id}:${editingProvider?.updated_at}`}
              onRevealCredential={handleRevealCredential}
            />
          </div>
        </DialogContent>
      </Dialog>
    </div>
  )
}
