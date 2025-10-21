import { useEffect, useState } from 'react'
import { useParams, useNavigate } from 'react-router-dom'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { useForm } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { toast } from 'sonner'
import {
  getNotificationProviderOptions,
  updateEmailProviderMutation,
  updateSlackProviderMutation,
  testProviderMutation,
  deleteProvider2Mutation,
} from '@/api/client/@tanstack/react-query.gen'
import { ProviderForm } from '@/components/monitoring/ProviderForm'
import {
  ProviderFormData,
  providerSchema,
} from '@/components/monitoring/schemas'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Alert, AlertDescription } from '@/components/ui/alert'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { ArrowLeft, Trash2, TestTube, Loader2 } from 'lucide-react'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'

export function EditNotificationProvider() {
  const { id } = useParams<{ id: string }>()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { setBreadcrumbs } = useBreadcrumbs()
  const [showDeleteDialog, setShowDeleteDialog] = useState(false)

  usePageTitle('Edit Notification Provider')

  // Fetch provider data
  const {
    data: provider,
    isLoading,
    error,
  } = useQuery({
    ...getNotificationProviderOptions({ path: { id: parseInt(id || '0') } }),
    enabled: !!id,
  })

  useEffect(() => {
    if (provider) {
      setBreadcrumbs([
        { label: 'Monitoring & Alerts', href: '/monitoring' },
        { label: 'Providers', href: '/monitoring/providers' },
        { label: provider.name || 'Edit Provider' },
      ])
    }
  }, [setBreadcrumbs, provider])

  const form = useForm<ProviderFormData>({
    resolver: zodResolver(providerSchema),
    defaultValues: {
      name: '',
      provider_type: 'email',
      config: {
        // Slack config
        webhook_url: '',
        channel: '',
        // Email config
        smtp_host: '',
        smtp_port: 587,
        use_credentials: false,
        smtp_username: '',
        password: '',
        from_name: '',
        from_address: '',
        to_addresses: [], // Array of email addresses
        tls_mode: 'Starttls', // Default TLS mode
        starttls_required: false,
        accept_invalid_certs: false,
      },
    },
  })

  // Load provider data into form
  useEffect(() => {
    if (provider) {
      form.reset({
        name: provider.name,
        provider_type: provider.provider_type,
        config: {
          // Slack config
          webhook_url: provider.config.webhook_url,
          channel: provider.config.channel,

          // Email config
          smtp_host: provider.config.smtp_host,
          smtp_port: provider.config.smtp_port,
          use_credentials: !!(
            provider.config.username || provider.config.password
          ),
          smtp_username: provider.config.username,
          password: provider.config.password,
          from_name: provider.config.from_name,
          from_address: provider.config.from_address,
          to_addresses: provider.config.to_addresses,
          tls_mode: provider.config.tls_mode || undefined,
          starttls_required: provider.config.starttls_required,
          accept_invalid_certs: provider.config.accept_invalid_certs,
        },
      })
    }
  }, [provider, form])

  // Update mutations
  const updateEmailMutation = useMutation({
    ...updateEmailProviderMutation(),
    meta: {
      errorTitle: 'Failed to update email provider',
    },
    onSuccess: () => {
      toast.success('Email provider updated successfully')
      queryClient.invalidateQueries({ queryKey: ['getNotificationProviders'] })
      navigate('/monitoring/providers')
    },
  })

  const updateSlackMutation = useMutation({
    ...updateSlackProviderMutation(),
    meta: {
      errorTitle: 'Failed to update Slack provider',
    },
    onSuccess: () => {
      toast.success('Slack provider updated successfully')
      queryClient.invalidateQueries({ queryKey: ['getNotificationProviders'] })
      navigate('/monitoring/providers')
    },
  })

  // Test mutation with toast.promise
  const testMutation = useMutation({
    ...testProviderMutation(),
    meta: {
      errorTitle: 'Failed to test provider',
    },
  })

  // Delete mutation
  const deleteMutation = useMutation({
    ...deleteProvider2Mutation(),
    meta: {
      errorTitle: 'Failed to delete provider',
    },
    onSuccess: () => {
      toast.success('Provider deleted successfully')
      queryClient.invalidateQueries({ queryKey: ['getNotificationProviders'] })
      navigate('/monitoring/providers')
    },
  })

  const handleSubmit = async (data: ProviderFormData) => {
    if (!provider) return

    const shouldSendCredentials = data.config.use_credentials

    if (data.provider_type === 'slack') {
      await updateSlackMutation.mutateAsync({
        path: { id: provider.id },
        body: {
          name: data.name,
          enabled: provider.enabled,
          config: {
            webhook_url: data.config.webhook_url!,
            channel: data.config.channel ?? null,
          },
        },
      })
    } else {
      await updateEmailMutation.mutateAsync({
        path: { id: provider.id },
        body: {
          name: data.name,
          enabled: provider.enabled,
          config: {
            smtp_host: data.config.smtp_host!,
            smtp_port: data.config.smtp_port!,
            username: shouldSendCredentials
              ? data.config.smtp_username || ''
              : '',
            password: shouldSendCredentials ? data.config.password || '' : '',
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

  const handleTest = async () => {
    if (!provider) return

    toast.promise(testMutation.mutateAsync({ path: { id: provider.id } }), {
      loading: 'Sending test notification...',
      success: 'Test notification sent successfully!',
      error: (error) => {
        const message =
          error?.response?.data?.detail || 'Failed to send test notification'
        return message
      },
    })
  }

  const handleDelete = async () => {
    if (!provider) return
    await deleteMutation.mutateAsync({ path: { id: provider.id } })
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center min-h-[400px]">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    )
  }

  if (error || !provider) {
    return (
      <div className="space-y-6">
        <Alert variant="destructive">
          <AlertDescription>
            Failed to load provider. Please check the ID and try again.
          </AlertDescription>
        </Alert>
        <Button
          onClick={() => navigate('/monitoring/providers')}
          variant="outline"
        >
          <ArrowLeft className="h-4 w-4 mr-2" />
          Back to Providers
        </Button>
      </div>
    )
  }

  const isSubmitting =
    updateEmailMutation.isPending || updateSlackMutation.isPending

  return (
    <div className="max-w-4xl mx-auto space-y-6">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-4">
          <Button
            onClick={() => navigate('/monitoring/providers')}
            variant="ghost"
            size="sm"
          >
            <ArrowLeft className="h-4 w-4 mr-2" />
            Back
          </Button>
          <div>
            <h1 className="text-3xl font-bold tracking-tight">Edit Provider</h1>
            <p className="text-muted-foreground">
              Update your notification provider settings
            </p>
          </div>
        </div>
        <div className="flex gap-2">
          <Button
            onClick={handleTest}
            variant="outline"
            disabled={testMutation.isPending}
          >
            <TestTube className="h-4 w-4 mr-2" />
            Test
          </Button>
          <Button
            onClick={() => setShowDeleteDialog(true)}
            variant="destructive"
            disabled={deleteMutation.isPending}
          >
            <Trash2 className="h-4 w-4 mr-2" />
            Delete
          </Button>
        </div>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Provider Configuration</CardTitle>
          <CardDescription>
            Update the configuration for your {provider.provider_type}{' '}
            notification provider
          </CardDescription>
        </CardHeader>
        <CardContent>
          <ProviderForm
            form={form}
            onSubmit={handleSubmit}
            isEdit={true}
            isLoading={isSubmitting}
          />
        </CardContent>
      </Card>

      <AlertDialog open={showDeleteDialog} onOpenChange={setShowDeleteDialog}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Are you sure?</AlertDialogTitle>
            <AlertDialogDescription>
              This will permanently delete the &quot;{provider.name}&quot;
              notification provider. This action cannot be undone.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={handleDelete}
              className="bg-destructive text-destructive-foreground"
            >
              Delete Provider
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}
