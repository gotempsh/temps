import { ProjectResponse } from '@/api/client'
import {
  deleteWebhookMutation,
  listWebhooksOptions,
} from '@/api/client/@tanstack/react-query.gen'
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
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  CheckCircle2,
  EllipsisVertical,
  Lock,
  Plus,
  Webhook,
  XCircle,
} from 'lucide-react'
import { useMemo, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'

interface WebhooksSettingsProps {
  project: ProjectResponse
}

export function WebhooksSettings({ project }: WebhooksSettingsProps) {
  const navigate = useNavigate()
  const [webhookToDelete, setWebhookToDelete] = useState<number | null>(null)

  const {
    data: webhooks,
    refetch: refetchWebhooks,
    isLoading,
    error,
  } = useQuery({
    ...listWebhooksOptions({
      path: {
        project_id: project.id,
      },
    }),
  })

  const deleteWebhook = useMutation({
    ...deleteWebhookMutation(),
    meta: {
      errorTitle: 'Failed to delete webhook',
    },
    onSuccess: () => {
      toast.success('Webhook deleted successfully')
      refetchWebhooks()
      setWebhookToDelete(null)
    },
  })

  const handleDelete = (webhookId: number) => {
    deleteWebhook.mutate({
      path: {
        project_id: project.id,
        webhook_id: webhookId,
      },
    })
  }

  const deleteDialogOpen = useMemo(
    () => webhookToDelete !== null,
    [webhookToDelete]
  )

  return (
    <div>
      <div className="flex items-center justify-between mb-4">
        <div>
          <h2 className="text-lg font-semibold">Webhooks</h2>
          <p className="text-sm text-muted-foreground mt-1">
            Configure webhooks to receive real-time notifications about events
            in your project
          </p>
        </div>
        <Button
          onClick={() =>
            navigate(`/projects/${project.slug}/settings/webhooks/new`)
          }
          disabled={isLoading}
        >
          <Plus className="h-4 w-4 mr-2" />
          Add Webhook
        </Button>
      </div>

      {error && (
        <ErrorAlert
          title="Failed to load webhooks"
          description={
            error instanceof Error
              ? error.message
              : 'An unexpected error occurred while loading webhooks'
          }
          retry={() => refetchWebhooks()}
        />
      )}

      {isLoading ? (
        <div className="space-y-4">
          {Array.from({ length: 3 }).map((_, i) => (
            <Card key={i}>
              <CardHeader className="pb-3">
                <div className="flex items-start justify-between">
                  <div className="flex items-start gap-3 flex-1">
                    <Skeleton className="h-5 w-5 mt-1" />
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2 mb-1">
                        <Skeleton className="h-4 w-64" />
                        <Skeleton className="h-5 w-16" />
                        <Skeleton className="h-5 w-20" />
                      </div>
                      <Skeleton className="h-3 w-48" />
                    </div>
                  </div>
                  <Skeleton className="h-8 w-8" />
                </div>
              </CardHeader>
              <CardContent>
                <div className="flex flex-wrap gap-1.5">
                  <Skeleton className="h-5 w-32" />
                  <Skeleton className="h-5 w-28" />
                  <Skeleton className="h-5 w-36" />
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      ) : !error && webhooks && webhooks.length > 0 ? (
        <div className="space-y-4">
          {webhooks.map((webhook) => (
            <Card
              key={webhook.id}
              className="cursor-pointer hover:border-primary/50 transition-colors"
              onClick={() =>
                navigate(
                  `/projects/${project.slug}/settings/webhooks/${webhook.id}`
                )
              }
            >
              <CardHeader className="pb-3">
                <div className="flex items-start justify-between">
                  <div className="flex items-start gap-3 flex-1">
                    <div className="mt-1">
                      <Webhook className="h-5 w-5 text-muted-foreground" />
                    </div>
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2 mb-1">
                        <CardTitle className="text-base font-mono text-sm break-all">
                          {webhook.url}
                        </CardTitle>
                        {webhook.enabled ? (
                          <Badge
                            variant="outline"
                            className="border-green-500/50 text-green-600 bg-green-50 dark:bg-green-950 dark:text-green-400"
                          >
                            <CheckCircle2 className="h-3 w-3 mr-1" />
                            Enabled
                          </Badge>
                        ) : (
                          <Badge
                            variant="outline"
                            className="border-gray-500/50 text-gray-600 bg-gray-50 dark:bg-gray-950 dark:text-gray-400"
                          >
                            <XCircle className="h-3 w-3 mr-1" />
                            Disabled
                          </Badge>
                        )}
                        {webhook.has_secret && (
                          <Badge variant="outline" className="gap-1">
                            <Lock className="h-3 w-3" />
                            Secured
                          </Badge>
                        )}
                      </div>
                      <CardDescription className="text-xs">
                        Created {new Date(webhook.created_at).toLocaleDateString()}
                        {webhook.updated_at !== webhook.created_at && (
                          <>
                            {' '}
                            • Updated{' '}
                            {new Date(webhook.updated_at).toLocaleDateString()}
                          </>
                        )}
                      </CardDescription>
                    </div>
                  </div>
                  <DropdownMenu>
                    <DropdownMenuTrigger asChild>
                      <Button
                        variant="ghost"
                        size="icon"
                        className="h-8 w-8"
                        onClick={(e) => e.stopPropagation()}
                      >
                        <EllipsisVertical className="h-4 w-4" />
                      </Button>
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end">
                      <DropdownMenuItem
                        onClick={() =>
                          navigate(
                            `/projects/${project.slug}/settings/webhooks/${webhook.id}`
                          )
                        }
                      >
                        View Details
                      </DropdownMenuItem>
                      <DropdownMenuItem
                        onClick={() =>
                          navigate(
                            `/projects/${project.slug}/settings/webhooks/${webhook.id}/edit`
                          )
                        }
                      >
                        Edit
                      </DropdownMenuItem>
                      <DropdownMenuSeparator />
                      <DropdownMenuItem
                        className="text-destructive"
                        onClick={() => setWebhookToDelete(webhook.id)}
                      >
                        Delete
                      </DropdownMenuItem>
                    </DropdownMenuContent>
                  </DropdownMenu>
                </div>
              </CardHeader>
              <CardContent>
                <div className="flex flex-wrap gap-1.5">
                  {webhook.events.map((event) => (
                    <Badge key={event} variant="secondary" className="text-xs">
                      {event}
                    </Badge>
                  ))}
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      ) : !error ? (
        <Card>
          <CardContent className="flex flex-col items-center justify-center py-12">
            <Webhook className="h-12 w-12 text-muted-foreground/50 mb-4" />
            <h3 className="text-lg font-semibold mb-2">No webhooks configured</h3>
            <p className="text-sm text-muted-foreground text-center mb-4 max-w-md">
              Webhooks allow you to receive real-time HTTP notifications about
              events in your project, such as deployments, errors, and more.
            </p>
            <Button
              onClick={() =>
                navigate(`/projects/${project.slug}/settings/webhooks/new`)
              }
            >
              <Plus className="h-4 w-4 mr-2" />
              Create Your First Webhook
            </Button>
          </CardContent>
        </Card>
      ) : null}

      <AlertDialog open={deleteDialogOpen} onOpenChange={() => setWebhookToDelete(null)}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete webhook?</AlertDialogTitle>
            <AlertDialogDescription>
              This action cannot be undone. This will permanently delete the
              webhook and stop all event notifications to this endpoint.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                if (webhookToDelete) {
                  handleDelete(webhookToDelete)
                }
              }}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              Delete
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}
