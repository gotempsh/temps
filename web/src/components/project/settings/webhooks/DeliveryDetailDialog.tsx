import { ProjectResponse, WebhookDeliveryResponse } from '@/api/client'
import { listDeliveriesOptions } from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { ScrollArea } from '@/components/ui/scroll-area'
import { Separator } from '@/components/ui/separator'
import { Skeleton } from '@/components/ui/skeleton'
import { useQuery } from '@tanstack/react-query'
import { AlertCircle, CheckCircle2, Clock, XCircle } from 'lucide-react'

interface DeliveryDetailDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  project: ProjectResponse
  webhookId: number
  deliveryId: number
}

function formatJSON(str: string | null | undefined): string {
  if (!str) return 'N/A'
  try {
    return JSON.stringify(JSON.parse(str), null, 2)
  } catch {
    return str
  }
}

export function DeliveryDetailDialog({
  open,
  onOpenChange,
  project,
  webhookId,
  deliveryId,
}: DeliveryDetailDialogProps) {
  const { data: deliveries, isLoading } = useQuery({
    ...listDeliveriesOptions({
      path: {
        project_id: project.id,
        webhook_id: webhookId,
      },
      query: {
        limit: 100,
      },
    }),
    enabled: open,
  })

  const delivery = deliveries?.find((d) => d.id === deliveryId)

  if (isLoading) {
    return (
      <Dialog open={open} onOpenChange={onOpenChange}>
        <DialogContent className="max-w-3xl max-h-[90vh]">
          <DialogHeader>
            <Skeleton className="h-6 w-48" />
            <Skeleton className="h-4 w-96 mt-2" />
          </DialogHeader>
          <div className="space-y-4">
            <Skeleton className="h-20 w-full" />
            <Skeleton className="h-40 w-full" />
          </div>
        </DialogContent>
      </Dialog>
    )
  }

  if (!delivery) {
    return null
  }

  const duration = delivery.delivered_at
    ? Math.round(
        (new Date(delivery.delivered_at).getTime() -
          new Date(delivery.created_at).getTime()) /
          1000
      )
    : null

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-3xl max-h-[90vh]">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-3">
            Delivery Details
            {delivery.success ? (
              <Badge
                variant="outline"
                className="gap-1 border-green-500/50 text-green-600 bg-green-50 dark:bg-green-950"
              >
                <CheckCircle2 className="h-3 w-3" />
                Success
              </Badge>
            ) : (
              <Badge
                variant="outline"
                className="gap-1 border-red-500/50 text-red-600 bg-red-50 dark:bg-red-950"
              >
                <XCircle className="h-3 w-3" />
                Failed
              </Badge>
            )}
          </DialogTitle>
          <DialogDescription>
            Event delivery information and response details
          </DialogDescription>
        </DialogHeader>

        <ScrollArea className="max-h-[calc(90vh-120px)]">
          <div className="space-y-6 pr-4">
            {/* Overview */}
            <div className="grid grid-cols-2 gap-4">
              <div>
                <div className="text-sm font-medium text-muted-foreground mb-1">
                  Event Type
                </div>
                <div className="font-mono text-sm">{delivery.event_type}</div>
              </div>
              <div>
                <div className="text-sm font-medium text-muted-foreground mb-1">
                  Event ID
                </div>
                <div className="flex items-center gap-2">
                  <code className="text-xs bg-muted px-1.5 py-0.5 rounded flex-1">
                    {delivery.event_id}
                  </code>
                  <CopyButton
                    value={delivery.event_id}
                    className="h-6 w-6 p-0"
                  />
                </div>
              </div>
              <div>
                <div className="text-sm font-medium text-muted-foreground mb-1">
                  Timestamp
                </div>
                <div className="flex items-center gap-1 text-sm">
                  <Clock className="h-3 w-3 text-muted-foreground" />
                  {new Date(delivery.created_at).toLocaleString()}
                </div>
              </div>
              <div>
                <div className="text-sm font-medium text-muted-foreground mb-1">
                  Duration
                </div>
                <div className="text-sm">
                  {duration !== null ? `${duration}s` : 'N/A'}
                </div>
              </div>
              <div>
                <div className="text-sm font-medium text-muted-foreground mb-1">
                  Status Code
                </div>
                <div className="font-mono text-sm">
                  {delivery.status_code || 'N/A'}
                </div>
              </div>
              <div>
                <div className="text-sm font-medium text-muted-foreground mb-1">
                  Attempt Number
                </div>
                <div className="text-sm">{delivery.attempt_number}</div>
              </div>
            </div>

            <Separator />

            {/* Error Message */}
            {delivery.error_message && (
              <>
                <div>
                  <div className="flex items-center gap-2 text-sm font-medium mb-2">
                    <AlertCircle className="h-4 w-4 text-red-600" />
                    Error Message
                  </div>
                  <div className="bg-red-50 dark:bg-red-950/30 border border-red-200 dark:border-red-900 rounded-lg p-3">
                    <code className="text-sm text-red-900 dark:text-red-300 whitespace-pre-wrap">
                      {delivery.error_message}
                    </code>
                  </div>
                </div>
                <Separator />
              </>
            )}

            {/* Response Body */}
            {delivery.response_body && (
              <div>
                <div className="flex items-center justify-between mb-2">
                  <div className="text-sm font-medium">Response Body</div>
                  <CopyButton
                    value={delivery.response_body}
                    className="h-7 px-2"
                  >
                    Copy
                  </CopyButton>
                </div>
                <div className="bg-muted rounded-lg p-4 overflow-auto">
                  <pre className="text-xs font-mono whitespace-pre-wrap break-all">
                    {formatJSON(delivery.response_body)}
                  </pre>
                </div>
              </div>
            )}

            {!delivery.response_body && !delivery.error_message && (
              <div className="text-center py-8 text-muted-foreground text-sm">
                No response data available
              </div>
            )}

            {/* Metadata */}
            <Separator />
            <div>
              <div className="text-sm font-medium mb-2">Delivery Metadata</div>
              <div className="text-xs text-muted-foreground space-y-1">
                <div>
                  Delivery ID: <code>{delivery.id}</code>
                </div>
                <div>
                  Webhook ID: <code>{delivery.webhook_id}</code>
                </div>
                {delivery.delivered_at && (
                  <div>
                    Delivered At:{' '}
                    {new Date(delivery.delivered_at).toLocaleString()}
                  </div>
                )}
              </div>
            </div>
          </div>
        </ScrollArea>

        <div className="flex justify-end pt-4 border-t">
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            Close
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  )
}
