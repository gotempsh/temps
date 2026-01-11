import { ProjectResponse } from '@/api/client'
import {
  getWebhookOptions,
  listDeliveriesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { useQuery } from '@tanstack/react-query'
import {
  ArrowLeft,
  CheckCircle2,
  Clock,
  Edit,
  ExternalLink,
  Lock,
  RefreshCw,
  XCircle,
} from 'lucide-react'
import { useState } from 'react'
import { useNavigate, useParams } from 'react-router-dom'
import { DeliveryDetailDialog } from './DeliveryDetailDialog'

interface WebhookDetailProps {
  project: ProjectResponse
}

export function WebhookDetail({ project }: WebhookDetailProps) {
  const navigate = useNavigate()
  const { webhookId } = useParams()
  const [selectedDeliveryId, setSelectedDeliveryId] = useState<number | null>(
    null
  )

  const {
    data: webhook,
    isLoading: isLoadingWebhook,
    error: webhookError,
    refetch: refetchWebhook,
  } = useQuery({
    ...getWebhookOptions({
      path: {
        project_id: project.id,
        webhook_id: Number(webhookId),
      },
    }),
    enabled: !!webhookId,
  })

  const {
    data: deliveries,
    isLoading: isLoadingDeliveries,
    refetch: refetchDeliveries,
  } = useQuery({
    ...listDeliveriesOptions({
      path: {
        project_id: project.id,
        webhook_id: Number(webhookId),
      },
      query: {
        limit: 100,
      },
    }),
    enabled: !!webhookId,
    refetchInterval: 10000, // Refresh every 10 seconds
  })

  if (isLoadingWebhook) {
    return (
      <div className="space-y-6">
        <div className="flex items-center gap-4">
          <Button variant="ghost" size="icon" disabled>
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <div className="flex-1">
            <Skeleton className="h-8 w-96 mb-2" />
            <Skeleton className="h-4 w-64" />
          </div>
        </div>
        <Card>
          <CardHeader>
            <Skeleton className="h-6 w-48" />
          </CardHeader>
          <CardContent className="space-y-4">
            <Skeleton className="h-4 w-full" />
            <Skeleton className="h-4 w-full" />
          </CardContent>
        </Card>
      </div>
    )
  }

  if (webhookError) {
    return (
      <div className="space-y-6">
        <div className="flex items-center gap-4">
          <Button
            variant="ghost"
            size="icon"
            onClick={() => navigate(`/projects/${project.slug}/settings/webhooks`)}
          >
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <div>
            <h2 className="text-2xl font-bold tracking-tight">Webhook Details</h2>
          </div>
        </div>
        <ErrorAlert
          title="Failed to load webhook"
          description={
            webhookError instanceof Error
              ? webhookError.message
              : 'An unexpected error occurred'
          }
          retry={() => refetchWebhook()}
        />
      </div>
    )
  }

  if (!webhook) {
    return null
  }

  const successCount = deliveries?.filter((d) => d.success).length || 0
  const failureCount = deliveries?.filter((d) => !d.success).length || 0
  const successRate =
    deliveries && deliveries.length > 0
      ? ((successCount / deliveries.length) * 100).toFixed(1)
      : '0'

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-4">
          <Button
            variant="ghost"
            size="icon"
            onClick={() => navigate(`/projects/${project.slug}/settings/webhooks`)}
          >
            <ArrowLeft className="h-4 w-4" />
          </Button>
          <div>
            <h2 className="text-2xl font-bold tracking-tight">
              Webhook Details
            </h2>
            <p className="text-muted-foreground text-sm font-mono">
              {webhook.url}
            </p>
          </div>
        </div>
        <Button
          onClick={() =>
            navigate(
              `/projects/${project.slug}/settings/webhooks/${webhookId}/edit`
            )
          }
        >
          <Edit className="h-4 w-4 mr-2" />
          Edit
        </Button>
      </div>

      <div className="grid gap-6 md:grid-cols-3">
        <Card>
          <CardHeader className="pb-3">
            <CardTitle className="text-sm font-medium">Status</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="flex items-center gap-2">
              {webhook.enabled ? (
                <>
                  <CheckCircle2 className="h-5 w-5 text-green-600" />
                  <span className="text-2xl font-bold">Enabled</span>
                </>
              ) : (
                <>
                  <XCircle className="h-5 w-5 text-gray-500" />
                  <span className="text-2xl font-bold">Disabled</span>
                </>
              )}
            </div>
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="pb-3">
            <CardTitle className="text-sm font-medium">Success Rate</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold">{successRate}%</div>
            <p className="text-xs text-muted-foreground mt-1">
              {successCount} successful, {failureCount} failed
            </p>
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="pb-3">
            <CardTitle className="text-sm font-medium">
              Total Deliveries
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold">
              {deliveries?.length || 0}
            </div>
            <p className="text-xs text-muted-foreground mt-1">
              Last 100 deliveries
            </p>
          </CardContent>
        </Card>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Configuration</CardTitle>
          <CardDescription>Webhook endpoint and settings</CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <div>
            <div className="text-sm font-medium mb-1">Endpoint URL</div>
            <div className="flex items-center gap-2">
              <code className="text-sm bg-muted px-2 py-1 rounded flex-1 font-mono">
                {webhook.url}
              </code>
              <Button
                variant="ghost"
                size="sm"
                onClick={() => window.open(webhook.url, '_blank')}
              >
                <ExternalLink className="h-4 w-4" />
              </Button>
            </div>
          </div>

          <div>
            <div className="text-sm font-medium mb-2">Subscribed Events</div>
            <div className="flex flex-wrap gap-1.5">
              {webhook.events.map((event) => (
                <Badge key={event} variant="secondary">
                  {event}
                </Badge>
              ))}
            </div>
          </div>

          <div className="flex items-center gap-2">
            <div className="text-sm font-medium">Security:</div>
            {webhook.has_secret ? (
              <Badge variant="outline" className="gap-1">
                <Lock className="h-3 w-3" />
                HMAC Signing Enabled
              </Badge>
            ) : (
              <Badge variant="outline" className="text-muted-foreground">
                No Secret Configured
              </Badge>
            )}
          </div>

          <div className="text-xs text-muted-foreground">
            <div>Created: {new Date(webhook.created_at).toLocaleString()}</div>
            {webhook.updated_at !== webhook.created_at && (
              <div>
                Updated: {new Date(webhook.updated_at).toLocaleString()}
              </div>
            )}
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-4">
          <div>
            <CardTitle>Recent Deliveries</CardTitle>
            <CardDescription>
              Delivery history and event invocations
            </CardDescription>
          </div>
          <Button
            variant="outline"
            size="sm"
            onClick={() => refetchDeliveries()}
            disabled={isLoadingDeliveries}
          >
            <RefreshCw
              className={`h-4 w-4 mr-2 ${isLoadingDeliveries ? 'animate-spin' : ''}`}
            />
            Refresh
          </Button>
        </CardHeader>
        <CardContent>
          {isLoadingDeliveries ? (
            <div className="space-y-2">
              {Array.from({ length: 5 }).map((_, i) => (
                <Skeleton key={i} className="h-12 w-full" />
              ))}
            </div>
          ) : deliveries && deliveries.length > 0 ? (
            <div className="border rounded-lg">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead className="w-[100px]">Status</TableHead>
                    <TableHead>Event</TableHead>
                    <TableHead>Event ID</TableHead>
                    <TableHead>Timestamp</TableHead>
                    <TableHead className="w-[100px]">Duration</TableHead>
                    <TableHead className="text-right w-[80px]">
                      Attempts
                    </TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {deliveries.map((delivery) => (
                    <TableRow
                      key={delivery.id}
                      className="cursor-pointer hover:bg-muted/50"
                      onClick={() => setSelectedDeliveryId(delivery.id)}
                    >
                      <TableCell>
                        {delivery.success ? (
                          <Badge
                            variant="outline"
                            className="gap-1 border-green-500/50 text-green-600 bg-green-50 dark:bg-green-950"
                          >
                            <CheckCircle2 className="h-3 w-3" />
                            {delivery.status_code}
                          </Badge>
                        ) : (
                          <Badge
                            variant="outline"
                            className="gap-1 border-red-500/50 text-red-600 bg-red-50 dark:bg-red-950"
                          >
                            <XCircle className="h-3 w-3" />
                            {delivery.status_code || 'Failed'}
                          </Badge>
                        )}
                      </TableCell>
                      <TableCell className="font-medium">
                        {delivery.event_type}
                      </TableCell>
                      <TableCell>
                        <code className="text-xs bg-muted px-1.5 py-0.5 rounded">
                          {delivery.event_id}
                        </code>
                      </TableCell>
                      <TableCell>
                        <div className="flex items-center gap-1 text-sm">
                          <Clock className="h-3 w-3 text-muted-foreground" />
                          {new Date(delivery.created_at).toLocaleString()}
                        </div>
                      </TableCell>
                      <TableCell>
                        {delivery.delivered_at ? (
                          <span className="text-sm text-muted-foreground">
                            {Math.round(
                              (new Date(delivery.delivered_at).getTime() -
                                new Date(delivery.created_at).getTime()) /
                                1000
                            )}
                            s
                          </span>
                        ) : (
                          <span className="text-sm text-muted-foreground">
                            N/A
                          </span>
                        )}
                      </TableCell>
                      <TableCell className="text-right">
                        {delivery.attempt_number}
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          ) : (
            <div className="flex flex-col items-center justify-center py-12 text-center">
              <Clock className="h-12 w-12 text-muted-foreground/50 mb-4" />
              <h3 className="text-lg font-semibold mb-2">No deliveries yet</h3>
              <p className="text-sm text-muted-foreground max-w-md">
                Webhook deliveries will appear here when events matching your
                subscriptions occur in this project.
              </p>
            </div>
          )}
        </CardContent>
      </Card>

      {selectedDeliveryId && (
        <DeliveryDetailDialog
          open={!!selectedDeliveryId}
          onOpenChange={(open) => !open && setSelectedDeliveryId(null)}
          project={project}
          webhookId={Number(webhookId)}
          deliveryId={selectedDeliveryId}
        />
      )}
    </div>
  )
}
