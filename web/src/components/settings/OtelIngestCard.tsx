import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import {
  getSettingsOptions,
  getSettingsQueryKey,
  updateOtelIngestMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { useAuth } from '@/contexts/AuthContext'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { AlertTriangle, Activity, Lock } from 'lucide-react'
import { toast } from 'sonner'

export function OtelIngestCard() {
  const queryClient = useQueryClient()
  const { user } = useAuth()
  const { data: settings, isLoading, error } = useQuery(getSettingsOptions())

  const canWrite = user?.role === 'admin' || user?.role === 'user'

  const updateMutation = useMutation({
    ...updateOtelIngestMutation(),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: getSettingsQueryKey() })
    },
    onError: (_err: unknown) => {
      toast.error('Failed to update OpenTelemetry ingestion')
    },
  } as Parameters<typeof useMutation>[0])

  if (isLoading) {
    return (
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Activity className="h-5 w-5" />
            OpenTelemetry Ingestion
          </CardTitle>
          <CardDescription>
            Accept OTLP metrics, traces, and logs pushed by your applications.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <Skeleton className="h-16 w-full" />
        </CardContent>
      </Card>
    )
  }

  if (error || !settings) {
    return (
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Activity className="h-5 w-5" />
            OpenTelemetry Ingestion
          </CardTitle>
        </CardHeader>
        <CardContent>
          <Alert variant="destructive">
            <AlertTriangle className="h-4 w-4" />
            <AlertTitle>Failed to load settings</AlertTitle>
            <AlertDescription>
              The server returned an error. Check the console logs or contact
              your administrator.
            </AlertDescription>
          </Alert>
        </CardContent>
      </Card>
    )
  }

  const enabled = settings.otel_ingest_enabled ?? true

  const onToggle = (next: boolean) => {
    updateMutation.mutate(
      { body: { enabled: next } },
      {
        onSuccess: () =>
          toast.success(
            next
              ? 'OpenTelemetry ingestion enabled'
              : 'OpenTelemetry ingestion disabled'
          ),
      }
    )
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Activity className="h-5 w-5" />
          OpenTelemetry Ingestion
        </CardTitle>
        <CardDescription>
          Accept OTLP/HTTP metrics, traces, and logs pushed by your
          applications and services (<code>/api/otel/v1/*</code>). Disabling is
          a kill-switch: exporters get a normal success response (no retry
          storms), nothing is stored, and the anomaly/health analysis loops
          pause — reclaiming CPU when you don't need telemetry. This does not
          affect the separate resource-metrics scraper.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {!canWrite && (
          <Alert>
            <Lock className="h-4 w-4" />
            <AlertTitle>Read-only</AlertTitle>
            <AlertDescription>
              Your role can view this setting but not change it. The{' '}
              <code>otel:write</code> permission is required.
            </AlertDescription>
          </Alert>
        )}

        <div className="flex items-start justify-between rounded-lg border p-3">
          <div className="space-y-0.5">
            <Label htmlFor="otel-ingest-enabled" className="text-sm">
              Accept OTLP ingest
            </Label>
            <p className="text-xs text-muted-foreground max-w-prose">
              When off, telemetry pushed to the ingest endpoints is accepted
              and discarded.
            </p>
          </div>
          <Switch
            id="otel-ingest-enabled"
            checked={enabled}
            disabled={!canWrite || updateMutation.isPending}
            onCheckedChange={onToggle}
          />
        </div>
      </CardContent>
    </Card>
  )
}
