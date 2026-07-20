'use client'

import {
  getEmailTrackingStatus,
  setupEmailTracking,
  type EmailTrackingStatusResponse,
} from '@/api/client'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import { CopyButton } from '@/components/ui/copy-button'
import { Skeleton } from '@/components/ui/skeleton'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { formatDistanceToNow } from 'date-fns'
import {
  AlertCircle,
  CheckCircle2,
  ChevronsUpDown,
  Clock,
  Loader2,
  Wand2,
} from 'lucide-react'
import { toast } from 'sonner'

function problemMessage(error: unknown, fallback: string): string {
  if (error && typeof error === 'object' && 'detail' in error) {
    const detail = (error as { detail?: unknown }).detail
    if (typeof detail === 'string' && detail.length > 0) {
      return detail
    }
  }
  return fallback
}

async function fetchTrackingStatus(
  providerId: number
): Promise<EmailTrackingStatusResponse> {
  const response = await getEmailTrackingStatus({ path: { id: providerId } })
  if (response.error || !response.data) {
    throw new Error(
      problemMessage(response.error, 'Failed to load event tracking status')
    )
  }
  return response.data
}

/// Hosts SNS can never reach — a subscription against these silently
/// receives nothing, so warn before the user burns time debugging AWS.
function isNonPublicUrl(url: string): boolean {
  try {
    const host = new URL(url).hostname
    return (
      host === 'localhost' ||
      host === 'localho.st' ||
      host.endsWith('.local') ||
      host.endsWith('.internal') ||
      /^127\./.test(host) ||
      /^10\./.test(host) ||
      /^192\.168\./.test(host) ||
      /^172\.(1[6-9]|2\d|3[01])\./.test(host)
    )
  } catch {
    return true
  }
}

// Setup-time-only actions (sns/ses:*). At runtime SNS pushes to the
// webhook on its own — Temps' credentials are not involved, so no extra
// permissions are needed once setup has run.
const SETUP_IAM_POLICY = `{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": [
        "sns:CreateTopic",
        "sns:Subscribe",
        "sesv2:CreateConfigurationSet",
        "sesv2:CreateConfigurationSetEventDestination",
        "sesv2:UpdateConfigurationSetEventDestination"
      ],
      "Resource": [
        "arn:aws:sns:*:*:temps-email-events-*",
        "arn:aws:ses:*:*:configuration-set/temps-tracking"
      ]
    }
  ]
}`

export function EmailTrackingSetup({ providerId }: { providerId: number }) {
  const queryClient = useQueryClient()

  const {
    data: status,
    isLoading,
    error,
  } = useQuery({
    queryKey: ['email-tracking-status', providerId],
    queryFn: () => fetchTrackingStatus(providerId),
  })

  const setupMutation = useMutation({
    mutationFn: async () => {
      const response = await setupEmailTracking({ path: { id: providerId } })
      if (response.error || !response.data) {
        throw new Error(
          problemMessage(response.error, 'Event tracking setup failed')
        )
      }
      return response.data
    },
    onSuccess: (result) => {
      toast.success(
        `Event tracking configured — topic ${result.topic_arn} is now bound to this provider`
      )
      queryClient.invalidateQueries({
        queryKey: ['email-tracking-status', providerId],
      })
      queryClient.invalidateQueries({ queryKey: ['email-providers'] })
    },
    onError: (err: Error) => {
      toast.error(err.message)
    },
  })

  if (isLoading) {
    return (
      <div className="space-y-2 rounded-md border p-4">
        <Skeleton className="h-4 w-40" />
        <Skeleton className="h-3 w-64" />
      </div>
    )
  }

  if (error) {
    return (
      <Alert variant="destructive">
        <AlertCircle className="h-4 w-4" />
        <AlertTitle>Event tracking status unavailable</AlertTitle>
        <AlertDescription>
          {error instanceof Error
            ? error.message
            : 'Failed to load event tracking status.'}
        </AlertDescription>
      </Alert>
    )
  }

  if (!status || !status.supports_event_tracking) {
    return null
  }

  const confirmed = !!status.subscription_confirmed_at
  const hasTopic = !!status.sns_topic_arn
  const nonPublic = isNonPublicUrl(status.webhook_url)

  return (
    <div className="space-y-3 rounded-md border p-4">
      <div className="flex items-center justify-between gap-2">
        <div>
          <p className="text-sm font-medium">Delivery event tracking</p>
          <p className="text-xs text-muted-foreground">
            Bounces, complaints, and deliveries reported by AWS via SNS.
          </p>
        </div>
        {hasTopic ? (
          confirmed ? (
            <Badge variant="default" className="gap-1">
              <CheckCircle2 className="h-3 w-3" />
              Subscription confirmed
            </Badge>
          ) : (
            <Badge variant="secondary" className="gap-1">
              <Clock className="h-3 w-3" />
              Subscription pending
            </Badge>
          )
        ) : (
          <Badge variant="outline">Not configured</Badge>
        )}
      </div>

      {nonPublic && (
        <Alert variant="destructive">
          <AlertCircle className="h-4 w-4" />
          <AlertTitle>Instance is not publicly reachable</AlertTitle>
          <AlertDescription>
            The webhook URL ({status.webhook_url}) points at a private address.
            AWS SNS cannot deliver events to it — set a public external URL in
            the platform settings first.
          </AlertDescription>
        </Alert>
      )}

      {hasTopic && !confirmed && (
        <Alert>
          <Clock className="h-4 w-4" />
          <AlertTitle>Waiting for AWS to confirm the subscription</AlertTitle>
          <AlertDescription>
            This normally completes within seconds of setup. If it stays
            pending: the webhook must be reachable from the internet, and the
            topic ARN must have been saved here <em>before</em> the endpoint was
            subscribed — if you subscribed first, delete that subscription in
            the AWS console and run setup again.
          </AlertDescription>
        </Alert>
      )}

      <div className="space-y-1 text-xs text-muted-foreground">
        {hasTopic && (
          <p className="break-all">
            Topic: <span className="font-mono">{status.sns_topic_arn}</span>
          </p>
        )}
        <p>
          Last provider event:{' '}
          {status.last_event_at
            ? formatDistanceToNow(new Date(status.last_event_at), {
                addSuffix: true,
              })
            : 'none received yet'}
        </p>
      </div>

      <div className="flex items-center gap-2">
        <Button
          type="button"
          size="sm"
          onClick={() => setupMutation.mutate()}
          disabled={setupMutation.isPending || nonPublic}
        >
          {setupMutation.isPending ? (
            <Loader2 className="mr-2 h-4 w-4 animate-spin" />
          ) : (
            <Wand2 className="mr-2 h-4 w-4" />
          )}
          {hasTopic ? 'Re-run setup' : 'Set up automatically'}
        </Button>
        <p className="text-xs text-muted-foreground">
          Creates the SNS topic, webhook subscription, and SES event destination
          using this provider&apos;s credentials.
        </p>
      </div>

      <Collapsible>
        <CollapsibleTrigger asChild>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="gap-1 px-2"
          >
            <ChevronsUpDown className="h-3 w-3" />
            Manual setup &amp; required IAM permissions
          </Button>
        </CollapsibleTrigger>
        <CollapsibleContent className="space-y-3 pt-2 text-xs">
          <ol className="list-decimal space-y-2 pl-4">
            <li>
              Create an SNS topic in{' '}
              <span className="font-mono">this provider&apos;s region</span>.
            </li>
            <li>
              Paste its ARN into the &quot;SNS Topic ARN&quot; field above and{' '}
              <strong>save the provider first</strong> — Temps only
              auto-confirms subscriptions for topics it already knows about.
              Subscribing before saving leaves the subscription stuck in
              &quot;pending&quot; forever.
            </li>
            <li>
              In SES, attach an event destination for bounces, complaints, and
              deliveries to the{' '}
              <span className="font-mono">temps-tracking</span> configuration
              set, publishing to that topic.
            </li>
            <li className="space-y-1">
              <span>Subscribe this webhook endpoint (HTTPS) to the topic:</span>
              <span className="flex items-center gap-1">
                <code className="break-all rounded bg-muted px-1.5 py-0.5">
                  {status.webhook_url}
                </code>
                <CopyButton
                  value={status.webhook_url}
                  className="h-6 w-6 shrink-0"
                />
              </span>
            </li>
          </ol>
          <div className="space-y-1">
            <div className="flex items-center justify-between">
              <p className="font-medium">
                IAM policy for automatic setup (setup-time only)
              </p>
              <CopyButton value={SETUP_IAM_POLICY} className="h-6 w-6" />
            </div>
            <pre className="max-h-48 overflow-auto rounded bg-muted p-2 font-mono text-[11px] leading-snug">
              {SETUP_IAM_POLICY}
            </pre>
            <p className="text-muted-foreground">
              Runtime needs no extra permissions — SNS pushes events to the
              webhook without using this provider&apos;s credentials. Sending
              only requires <span className="font-mono">ses:SendEmail</span> /{' '}
              <span className="font-mono">ses:SendRawEmail</span> as before.
            </p>
          </div>
        </CollapsibleContent>
      </Collapsible>
    </div>
  )
}
