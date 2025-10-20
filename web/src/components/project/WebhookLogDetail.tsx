import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { ArrowLeftIcon, RotateCwIcon } from 'lucide-react'
import { toast } from 'sonner'
import { useState } from 'react'

// Mock detailed webhook data
const mockWebhookDetail = {
  id: 'evt_1OqXy2CZ6qsJgndZ',
  event: 'payment_intent.succeeded',
  status: 'success',
  timestamp: '2024-03-20T15:30:00Z',
  responseStatus: 200,
  responseTime: '234ms',
  requestId: 'req_1OqXy2CZ6qsJgndZ',
  environment: 'production',
  request: {
    headers: {
      'stripe-signature':
        't=1679328600,v1=5257a869e7ecebeda32affa62cdca3fa51cad7e544',
      'content-type': 'application/json',
      'user-agent': 'Stripe/1.0 (+https://stripe.com/webhooks)',
    },
    body: {
      id: 'evt_1OqXy2CZ6qsJgndZ',
      object: 'event',
      api_version: '2023-10-16',
      created: 1679328600,
      data: {
        object: {
          id: 'pi_3OqXy2CZ6qsJgndZ0K8m9BsL',
          amount: 2900,
          status: 'succeeded',
          currency: 'usd',
        },
      },
      type: 'payment_intent.succeeded',
      livemode: true,
    },
  },
  response: {
    headers: {
      'content-type': 'application/json',
      server: 'nginx',
      date: 'Wed, 20 Mar 2024 15:30:00 GMT',
    },
    body: {
      received: true,
    },
    statusCode: 200,
  },
}

interface WebhookLogDetailProps {
  webhookId: string
  onBack: () => void
}

export function WebhookLogDetail({ webhookId, onBack }: WebhookLogDetailProps) {
  const [isResending, setIsResending] = useState(false)

  const handleResend = async () => {
    setIsResending(true)
    // Mock implementation
    await new Promise((resolve) => setTimeout(resolve, 1000))
    toast.success('Webhook resent successfully')
    setIsResending(false)
  }

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex flex-col gap-6 sm:flex-row sm:items-center sm:justify-between">
        <div className="space-y-1">
          <Button variant="ghost" size="sm" className="mb-2" onClick={onBack}>
            <ArrowLeftIcon className="h-4 w-4 mr-2" />
            Back to Logs
          </Button>
          <h1 className="text-2xl font-bold">{mockWebhookDetail.event}</h1>
          <div className="flex items-center gap-2">
            <Badge
              variant={
                mockWebhookDetail.status === 'success'
                  ? 'default'
                  : 'destructive'
              }
              className="capitalize"
            >
              {mockWebhookDetail.status}
            </Badge>
            <span className="text-sm text-muted-foreground">
              {new Date(mockWebhookDetail.timestamp).toLocaleString()}
            </span>
          </div>
        </div>
        <Button onClick={handleResend} disabled={isResending}>
          <RotateCwIcon className="h-4 w-4 mr-2" />
          {isResending ? 'Resending...' : 'Resend Webhook'}
        </Button>
      </div>

      {/* Overview Card */}
      <Card>
        <CardHeader>
          <CardTitle>Overview</CardTitle>
        </CardHeader>
        <CardContent className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
          <div>
            <div className="text-sm font-medium text-muted-foreground">
              Status
            </div>
            <div className="mt-1">
              <Badge
                variant={
                  mockWebhookDetail.responseStatus >= 200 &&
                  mockWebhookDetail.responseStatus < 300
                    ? 'default'
                    : 'destructive'
                }
              >
                {mockWebhookDetail.responseStatus}
              </Badge>
            </div>
          </div>
          <div>
            <div className="text-sm font-medium text-muted-foreground">
              Response Time
            </div>
            <div className="mt-1 font-medium">
              {mockWebhookDetail.responseTime}
            </div>
          </div>
          <div>
            <div className="text-sm font-medium text-muted-foreground">
              Environment
            </div>
            <div className="mt-1">
              <Badge variant="secondary" className="capitalize">
                {mockWebhookDetail.environment}
              </Badge>
            </div>
          </div>
          <div>
            <div className="text-sm font-medium text-muted-foreground">
              Request ID
            </div>
            <div className="mt-1 font-mono text-sm">
              {mockWebhookDetail.requestId}
            </div>
          </div>
        </CardContent>
      </Card>

      {/* Request/Response Details */}
      <Card>
        <CardHeader>
          <CardTitle>Details</CardTitle>
        </CardHeader>
        <CardContent>
          <Tabs defaultValue="request" className="space-y-4">
            <TabsList>
              <TabsTrigger value="request">Request</TabsTrigger>
              <TabsTrigger value="response">Response</TabsTrigger>
            </TabsList>
            <TabsContent value="request" className="space-y-4">
              <div>
                <h3 className="text-sm font-medium mb-2">Headers</h3>
                <pre className="bg-muted rounded-md p-4 overflow-auto text-sm">
                  {JSON.stringify(mockWebhookDetail.request.headers, null, 2)}
                </pre>
              </div>
              <div>
                <h3 className="text-sm font-medium mb-2">Body</h3>
                <pre className="bg-muted rounded-md p-4 overflow-auto text-sm">
                  {JSON.stringify(mockWebhookDetail.request.body, null, 2)}
                </pre>
              </div>
            </TabsContent>
            <TabsContent value="response" className="space-y-4">
              <div>
                <h3 className="text-sm font-medium mb-2">Headers</h3>
                <pre className="bg-muted rounded-md p-4 overflow-auto text-sm">
                  {JSON.stringify(mockWebhookDetail.response.headers, null, 2)}
                </pre>
              </div>
              <div>
                <h3 className="text-sm font-medium mb-2">Body</h3>
                <pre className="bg-muted rounded-md p-4 overflow-auto text-sm">
                  {JSON.stringify(mockWebhookDetail.response.body, null, 2)}
                </pre>
              </div>
              <div>
                <h3 className="text-sm font-medium mb-2">Status Code</h3>
                <Badge
                  variant={
                    mockWebhookDetail.response.statusCode >= 200 &&
                    mockWebhookDetail.response.statusCode < 300
                      ? 'default'
                      : 'destructive'
                  }
                >
                  {mockWebhookDetail.response.statusCode}
                </Badge>
              </div>
            </TabsContent>
          </Tabs>
        </CardContent>
      </Card>
    </div>
  )
}
