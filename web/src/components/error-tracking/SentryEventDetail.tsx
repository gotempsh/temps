import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Label } from '@/components/ui/label'
import { ScrollArea } from '@/components/ui/scroll-area'
import { Switch } from '@/components/ui/switch'
import { cn } from '@/lib/utils'
import { SentryEvent, SentryException } from '@/types/sentry'
import { format, formatDistanceToNow } from 'date-fns'
import { CheckCircle, Copy } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { StackTrace } from './StackTrace'

interface SentryEventDetailProps {
  event: SentryEvent
  showRawData?: boolean
  defaultDetailedStackTrace?: boolean
  showHeader?: boolean
}

export function SentryEventDetail({
  event,
  showRawData = true,
  defaultDetailedStackTrace = false,
  showHeader = true,
}: SentryEventDetailProps) {
  const [showDetailedStackTrace, setShowDetailedStackTrace] = useState(
    defaultDetailedStackTrace
  )
  const [copiedSection, setCopiedSection] = useState<string | null>(null)

  const copyToClipboard = (text: string, section: string) => {
    navigator.clipboard.writeText(text).then(() => {
      setCopiedSection(section)
      toast.success('Copied to clipboard')
      setTimeout(() => setCopiedSection(null), 2000)
    })
  }

  const getSeverityColor = (level: string) => {
    switch (level?.toLowerCase()) {
      case 'error':
      case 'fatal':
        return 'text-red-600 bg-red-100 dark:bg-red-900/20'
      case 'warning':
        return 'text-yellow-600 bg-yellow-100 dark:bg-yellow-900/20'
      case 'info':
        return 'text-blue-600 bg-blue-100 dark:bg-blue-900/20'
      default:
        return 'text-gray-600 bg-gray-100 dark:bg-gray-900/20'
    }
  }

  const getLevelColor = (level: string) => {
    switch (level?.toLowerCase()) {
      case 'error':
        return 'destructive'
      case 'warning':
        return 'secondary'
      case 'info':
        return 'outline'
      default:
        return 'outline'
    }
  }

  const sentryData = event.sentry
  const exceptions = sentryData.exception?.values || []
  const mainException = exceptions[0]
  const breadcrumbs = sentryData.breadcrumbs?.values || []
  const contexts = sentryData.contexts || {}

  // Helper function to convert Sentry stack frames to our StackTrace component format
  const convertStackFrames = (exception: SentryException) => {
    return (
      exception?.stacktrace?.frames?.map((frame) => ({
        filename: frame.filename || frame.abs_path,
        function: frame.function,
        lineno: frame.lineno,
        colno: frame.colno,
        module: frame.module,
        in_app: frame.in_app,
        pre_context: frame.pre_context,
        context_line: frame.context_line,
        post_context: frame.post_context,
      })) || []
    )
  }

  return (
    <div className="space-y-6">
      {/* Event Header */}
      {showHeader && (
        <div>
          <div className="flex items-center gap-3 mb-2">
            <Badge className={cn(getSeverityColor(sentryData.level))}>
              {sentryData.level}
            </Badge>
            <h2 className="text-2xl font-semibold">
              {mainException?.value ||
                sentryData.logentry?.formatted ||
                'Event'}
            </h2>
          </div>
          <div className="flex items-center gap-4 text-sm text-muted-foreground">
            <span>{format(new Date(sentryData.timestamp * 1000), 'PPpp')}</span>
            <span>•</span>
            <span className="font-mono">ID: {sentryData.event_id}</span>
            {sentryData.transaction && (
              <>
                <span>•</span>
                <span>{sentryData.transaction}</span>
              </>
            )}
          </div>
        </div>
      )}

      <div className="grid grid-cols-1 lg:grid-cols-3 gap-6">
        {/* Main Content */}
        <div className="lg:col-span-2 space-y-4">
          {/* Log Entry */}
          {sentryData.logentry && exceptions.length === 0 && (
            <Card>
              <CardHeader>
                <CardTitle>Log Message</CardTitle>
              </CardHeader>
              <CardContent>
                <p className="font-mono text-sm bg-muted p-3 rounded">
                  {sentryData.logentry.formatted}
                </p>
              </CardContent>
            </Card>
          )}

          {/* All Exceptions with Stack Traces */}
          {exceptions.map((exception, exceptionIndex) => {
            const stackFrames = convertStackFrames(exception)
            return (
              <div key={exceptionIndex} className="space-y-4">
                {/* Exception Details - Information Dense */}
                <Card>
                  <CardHeader>
                    <div className="flex items-center justify-between">
                      <div>
                        <CardTitle>
                          {exceptions.length > 1
                            ? `Exception ${exceptionIndex + 1} of ${exceptions.length}`
                            : 'Exception Details'}
                        </CardTitle>
                        {exception.type && (
                          <CardDescription className="font-mono">
                            {exception.type}
                          </CardDescription>
                        )}
                      </div>
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() =>
                          copyToClipboard(
                            `${exception.type || 'Error'}: ${exception.value}`,
                            `exception-${exceptionIndex}`
                          )
                        }
                      >
                        {copiedSection === `exception-${exceptionIndex}` ? (
                          <CheckCircle className="h-4 w-4" />
                        ) : (
                          <Copy className="h-4 w-4" />
                        )}
                      </Button>
                    </div>
                  </CardHeader>
                  <CardContent>
                    {/* Exception Value */}
                    <div className="mb-4">
                      <div className="text-sm text-muted-foreground mb-1">
                        Error Message
                      </div>
                      <div className="font-mono text-sm bg-muted p-3 rounded">
                        {exception.value}
                      </div>
                    </div>

                    {/* Two Column Grid for Dense Information */}
                    <div className="grid grid-cols-2 gap-x-6 gap-y-3">
                      {/* Left Column */}
                      <div className="space-y-3">
                        {exception.module && (
                          <div>
                            <div className="text-xs text-muted-foreground mb-1">
                              Module
                            </div>
                            <div className="font-mono text-sm">
                              {exception.module}
                            </div>
                          </div>
                        )}

                        {exception.mechanism && (
                          <div>
                            <div className="text-xs text-muted-foreground mb-1">
                              Mechanism
                            </div>
                            <div className="flex flex-wrap gap-1">
                              <Badge variant="outline" className="text-xs">
                                {exception.mechanism.type}
                              </Badge>
                              <Badge
                                variant={
                                  exception.mechanism.handled
                                    ? 'secondary'
                                    : 'destructive'
                                }
                                className="text-xs"
                              >
                                {exception.mechanism.handled
                                  ? 'Handled'
                                  : 'Unhandled'}
                              </Badge>
                              {exception.mechanism.synthetic && (
                                <Badge variant="outline" className="text-xs">
                                  Synthetic
                                </Badge>
                              )}
                            </div>
                          </div>
                        )}

                        {sentryData.transaction && (
                          <div>
                            <div className="text-xs text-muted-foreground mb-1">
                              Transaction
                            </div>
                            <div className="font-mono text-xs break-all">
                              {sentryData.transaction}
                            </div>
                          </div>
                        )}

                        {sentryData.user && (
                          <div>
                            <div className="text-xs text-muted-foreground mb-1">
                              User
                            </div>
                            <div className="space-y-0.5">
                              {sentryData.user.email && (
                                <div className="font-mono text-xs">
                                  {sentryData.user.email}
                                </div>
                              )}
                              {sentryData.user.id && (
                                <div className="font-mono text-xs text-muted-foreground">
                                  ID: {sentryData.user.id}
                                </div>
                              )}
                              {sentryData.user.username && (
                                <div className="font-mono text-xs text-muted-foreground">
                                  @{sentryData.user.username}
                                </div>
                              )}
                            </div>
                          </div>
                        )}

                        {contexts.trace && (
                          <div>
                            <div className="text-xs text-muted-foreground mb-1">
                              Trace
                            </div>
                            <div className="font-mono text-xs break-all space-y-0.5">
                              <div>Trace: {contexts.trace.trace_id}</div>
                              <div className="text-muted-foreground">
                                Span: {contexts.trace.span_id}
                              </div>
                            </div>
                          </div>
                        )}
                      </div>

                      {/* Right Column */}
                      <div className="space-y-3">
                        {sentryData.request && (
                          <div>
                            <div className="text-xs text-muted-foreground mb-1">
                              Request
                            </div>
                            <div className="space-y-0.5">
                              <div className="flex items-center gap-2">
                                <Badge variant="outline" className="text-xs">
                                  {sentryData.request.method}
                                </Badge>
                              </div>
                              <div className="font-mono text-xs break-all">
                                {sentryData.request.url}
                              </div>
                            </div>
                          </div>
                        )}

                        {contexts.response && (
                          <div>
                            <div className="text-xs text-muted-foreground mb-1">
                              Response
                            </div>
                            <Badge
                              variant={
                                contexts.response.status_code
                                  ? contexts.response.status_code >= 500
                                    ? 'destructive'
                                    : contexts.response.status_code >= 400
                                      ? 'secondary'
                                      : 'outline'
                                  : 'outline'
                              }
                              className="text-xs"
                            >
                              {contexts.response.status_code || 'Unknown'}
                            </Badge>
                          </div>
                        )}

                        {sentryData.environment && (
                          <div>
                            <div className="text-xs text-muted-foreground mb-1">
                              Environment
                            </div>
                            <div className="text-sm">
                              {sentryData.environment}
                            </div>
                          </div>
                        )}

                        {sentryData.release && (
                          <div>
                            <div className="text-xs text-muted-foreground mb-1">
                              Release
                            </div>
                            <div className="font-mono text-xs break-all">
                              {sentryData.release}
                            </div>
                          </div>
                        )}

                        {sentryData.server_name && (
                          <div>
                            <div className="text-xs text-muted-foreground mb-1">
                              Server
                            </div>
                            <div className="font-mono text-xs">
                              {sentryData.server_name}
                            </div>
                          </div>
                        )}

                        {contexts.runtime && (
                          <div>
                            <div className="text-xs text-muted-foreground mb-1">
                              Runtime
                            </div>
                            <div className="font-mono text-xs">
                              {contexts.runtime.name} {contexts.runtime.version}
                            </div>
                          </div>
                        )}

                        {contexts.os && (
                          <div>
                            <div className="text-xs text-muted-foreground mb-1">
                              OS
                            </div>
                            <div className="text-xs">
                              {contexts.os.name} {contexts.os.version}
                            </div>
                          </div>
                        )}
                      </div>
                    </div>
                  </CardContent>
                </Card>

                {/* Stack Trace for this Exception */}
                {stackFrames.length > 0 && (
                  <Card>
                    <CardHeader>
                      <div className="flex items-center justify-between">
                        <div>
                          <CardTitle>
                            {exceptions.length > 1
                              ? `Stack Trace ${exceptionIndex + 1}`
                              : 'Stack Trace'}
                          </CardTitle>
                          <CardDescription>
                            {stackFrames.length} frames
                          </CardDescription>
                        </div>
                        <div className="flex items-center gap-4">
                          <div className="flex items-center gap-2">
                            <Label
                              htmlFor={`detailed-stack-${exceptionIndex}`}
                              className="text-sm font-normal"
                            >
                              Detailed
                            </Label>
                            <Switch
                              id={`detailed-stack-${exceptionIndex}`}
                              checked={showDetailedStackTrace}
                              onCheckedChange={setShowDetailedStackTrace}
                            />
                          </div>
                          <Button
                            variant="ghost"
                            size="sm"
                            onClick={() =>
                              copyToClipboard(
                                JSON.stringify(stackFrames, null, 2),
                                `stack-${exceptionIndex}`
                              )
                            }
                          >
                            {copiedSection === `stack-${exceptionIndex}` ? (
                              <CheckCircle className="h-4 w-4" />
                            ) : (
                              <Copy className="h-4 w-4" />
                            )}
                          </Button>
                        </div>
                      </div>
                    </CardHeader>
                    <CardContent>
                      <ScrollArea className="h-[600px]">
                        <StackTrace
                          frames={stackFrames}
                          detailed={showDetailedStackTrace}
                        />
                      </ScrollArea>
                    </CardContent>
                  </Card>
                )}
              </div>
            )
          })}

          {/* Spans (Performance Tracing) */}
          {sentryData.spans && sentryData.spans.length > 0 && (
            <Card>
              <CardHeader>
                <CardTitle>Spans</CardTitle>
                <CardDescription>
                  Performance tracing spans ({sentryData.spans.length} spans)
                </CardDescription>
              </CardHeader>
              <CardContent>
                <ScrollArea className="h-[400px]">
                  <div className="space-y-3">
                    {sentryData.spans.map((span, index) => (
                      <div
                        key={index}
                        className="border rounded-lg p-3 hover:bg-muted/50 transition-colors"
                      >
                        <div className="space-y-2">
                          <div className="flex items-center justify-between">
                            <div className="flex items-center gap-2">
                              <Badge variant="outline" className="text-xs">
                                {span.op || 'unknown'}
                              </Badge>
                              {span.status && (
                                <Badge
                                  variant={
                                    span.status === 'ok'
                                      ? 'outline'
                                      : 'destructive'
                                  }
                                  className="text-xs"
                                >
                                  {span.status}
                                </Badge>
                              )}
                            </div>
                            {span.start_timestamp && span.timestamp && (
                              <span className="text-xs text-muted-foreground">
                                {(
                                  (span.timestamp - span.start_timestamp) *
                                  1000
                                ).toFixed(2)}
                                ms
                              </span>
                            )}
                          </div>
                          {span.description && (
                            <p className="text-sm font-mono">
                              {span.description}
                            </p>
                          )}
                          <div className="flex items-center gap-2 text-xs text-muted-foreground">
                            <span className="font-mono">
                              Span: {span.span_id.slice(0, 8)}
                            </span>
                            {span.parent_span_id && (
                              <>
                                <span>→</span>
                                <span className="font-mono">
                                  Parent: {span.parent_span_id.slice(0, 8)}
                                </span>
                              </>
                            )}
                          </div>
                        </div>
                      </div>
                    ))}
                  </div>
                </ScrollArea>
              </CardContent>
            </Card>
          )}

          {/* Breadcrumbs */}
          {breadcrumbs.length > 0 && (
            <Card>
              <CardHeader>
                <CardTitle>Breadcrumbs</CardTitle>
                <CardDescription>
                  Events leading up to this error ({breadcrumbs.length} events)
                </CardDescription>
              </CardHeader>
              <CardContent>
                <ScrollArea className="h-[400px]">
                  <div className="space-y-2">
                    {[...breadcrumbs]
                      .map((breadcrumb, originalIndex) => ({
                        ...breadcrumb,
                        originalIndex,
                      }))
                      .sort((a, b) => {
                        const timeDiff = (b.timestamp || 0) - (a.timestamp || 0)
                        return timeDiff !== 0
                          ? timeDiff
                          : b.originalIndex - a.originalIndex
                      })
                      .map((breadcrumb, index) => (
                        <div
                          key={index}
                          className="flex items-start gap-3 p-2 hover:bg-muted/50 rounded transition-colors"
                        >
                          <div className="flex-shrink-0 mt-0.5">
                            <div className="h-2 w-2 rounded-full bg-muted-foreground/50" />
                          </div>
                          <div className="flex-1 min-w-0">
                            <div className="flex items-center gap-2 mb-1">
                              <Badge variant="outline" className="text-xs">
                                {breadcrumb.category || 'unknown'}
                              </Badge>
                              {breadcrumb.level && (
                                <Badge
                                  variant={getLevelColor(breadcrumb.level)}
                                  className="text-xs"
                                >
                                  {breadcrumb.level}
                                </Badge>
                              )}
                              <span className="text-xs text-muted-foreground">
                                {breadcrumb.timestamp
                                  ? formatDistanceToNow(
                                      new Date(breadcrumb.timestamp * 1000),
                                      { addSuffix: true }
                                    )
                                  : 'Unknown time'}
                              </span>
                            </div>
                            <p className="text-sm font-mono break-all">
                              {breadcrumb.message || 'No message'}
                            </p>
                            {breadcrumb.data && (
                              <div className="mt-1 pl-2 text-xs text-muted-foreground">
                                {Object.entries(breadcrumb.data).map(
                                  ([key, value]) => (
                                    <div key={key}>
                                      <span className="font-medium">
                                        {key}:
                                      </span>{' '}
                                      {typeof value === 'object'
                                        ? JSON.stringify(value)
                                        : String(value)}
                                    </div>
                                  )
                                )}
                              </div>
                            )}
                          </div>
                        </div>
                      ))}
                  </div>
                </ScrollArea>
              </CardContent>
            </Card>
          )}

          {/* Raw Event Data */}
          {showRawData && (
            <Card>
              <CardHeader>
                <div className="flex items-center justify-between">
                  <CardTitle>Raw Event Data</CardTitle>
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() =>
                      copyToClipboard(JSON.stringify(event, null, 2), 'raw')
                    }
                  >
                    {copiedSection === 'raw' ? (
                      <CheckCircle className="h-4 w-4" />
                    ) : (
                      <Copy className="h-4 w-4" />
                    )}
                  </Button>
                </div>
                <CardDescription>Complete event JSON data</CardDescription>
              </CardHeader>
              <CardContent>
                <ScrollArea className="h-[400px]">
                  <pre className="text-xs">
                    {JSON.stringify(event, null, 2)}
                  </pre>
                </ScrollArea>
              </CardContent>
            </Card>
          )}
        </div>

        {/* Sidebar */}
        <div className="space-y-4">
          {/* SDK Info */}
          {sentryData.sdk && (
            <Card>
              <CardHeader className="pb-3">
                <CardTitle className="text-sm font-medium">SDK</CardTitle>
              </CardHeader>
              <CardContent className="space-y-2">
                <div className="text-sm">
                  <div className="text-muted-foreground text-xs">Name</div>
                  <div className="font-mono">{sentryData.sdk.name}</div>
                </div>
                <div className="text-sm">
                  <div className="text-muted-foreground text-xs">Version</div>
                  <div className="font-mono">{sentryData.sdk.version}</div>
                </div>
                {sentryData.sdk.integrations &&
                  sentryData.sdk.integrations.length > 0 && (
                    <div className="text-sm">
                      <div className="text-muted-foreground text-xs mb-1">
                        Integrations ({sentryData.sdk.integrations.length})
                      </div>
                      <ScrollArea className="h-[100px]">
                        <div className="flex flex-wrap gap-1">
                          {sentryData.sdk.integrations.map((integration) => (
                            <Badge
                              key={integration}
                              variant="outline"
                              className="text-xs"
                            >
                              {integration}
                            </Badge>
                          ))}
                        </div>
                      </ScrollArea>
                    </div>
                  )}
              </CardContent>
            </Card>
          )}

          {/* Environment */}
          <Card>
            <CardHeader className="pb-3">
              <CardTitle className="text-sm font-medium">Environment</CardTitle>
            </CardHeader>
            <CardContent className="space-y-2">
              <div className="text-sm">
                <div className="text-muted-foreground text-xs">Platform</div>
                <div>{sentryData.platform}</div>
              </div>
              {sentryData.type && (
                <div className="text-sm">
                  <div className="text-muted-foreground text-xs">
                    Event Type
                  </div>
                  <Badge variant="outline">{sentryData.type}</Badge>
                </div>
              )}
              {sentryData.environment && (
                <div className="text-sm">
                  <div className="text-muted-foreground text-xs">
                    Environment
                  </div>
                  <div>{sentryData.environment}</div>
                </div>
              )}
              {sentryData.release && (
                <div className="text-sm">
                  <div className="text-muted-foreground text-xs">Release</div>
                  <div className="font-mono text-xs break-all">
                    {sentryData.release}
                  </div>
                </div>
              )}
              {sentryData.server_name && (
                <div className="text-sm">
                  <div className="text-muted-foreground text-xs">Server</div>
                  <div className="font-mono text-xs">
                    {sentryData.server_name}
                  </div>
                </div>
              )}
              {sentryData.logger && (
                <div className="text-sm">
                  <div className="text-muted-foreground text-xs">Logger</div>
                  <div className="font-mono text-xs">{sentryData.logger}</div>
                </div>
              )}
            </CardContent>
          </Card>

          {/* Transaction Info */}
          {(sentryData.transaction || sentryData.transaction_info) && (
            <Card>
              <CardHeader className="pb-3">
                <CardTitle className="text-sm font-medium">
                  Transaction
                </CardTitle>
              </CardHeader>
              <CardContent className="space-y-2">
                {sentryData.transaction && (
                  <div className="text-sm">
                    <div className="text-muted-foreground text-xs">Name</div>
                    <div className="font-mono text-xs break-all">
                      {sentryData.transaction}
                    </div>
                  </div>
                )}
                {sentryData.transaction_info?.source && (
                  <div className="text-sm">
                    <div className="text-muted-foreground text-xs">Source</div>
                    <Badge variant="outline">
                      {sentryData.transaction_info.source}
                    </Badge>
                  </div>
                )}
                {sentryData.start_timestamp && (
                  <div className="text-sm">
                    <div className="text-muted-foreground text-xs">
                      Duration
                    </div>
                    <div className="font-mono">
                      {(
                        (sentryData.timestamp - sentryData.start_timestamp) *
                        1000
                      ).toFixed(2)}{' '}
                      ms
                    </div>
                  </div>
                )}
              </CardContent>
            </Card>
          )}

          {/* Request Info */}
          {sentryData.request && (
            <Card>
              <CardHeader className="pb-3">
                <CardTitle className="text-sm font-medium">Request</CardTitle>
              </CardHeader>
              <CardContent className="space-y-2">
                {sentryData.request.method && (
                  <div className="text-sm">
                    <div className="text-muted-foreground text-xs">Method</div>
                    <Badge variant="outline" className="font-mono">
                      {sentryData.request.method}
                    </Badge>
                  </div>
                )}
                {sentryData.request.url && (
                  <div className="text-sm">
                    <div className="text-muted-foreground text-xs mb-1">
                      URL
                    </div>
                    <div className="font-mono text-xs break-all">
                      {sentryData.request.url}
                    </div>
                  </div>
                )}
                {sentryData.request.headers &&
                  sentryData.request.headers.length > 0 && (
                    <div className="text-sm">
                      <div className="text-muted-foreground text-xs mb-1">
                        Headers
                      </div>
                      <ScrollArea className="h-[150px]">
                        <div className="space-y-1 text-xs">
                          {sentryData.request.headers.map(
                            ([key, value], index) => (
                              <div key={index} className="flex gap-2">
                                <span className="font-mono font-medium">
                                  {key}:
                                </span>
                                <span className="font-mono text-muted-foreground break-all">
                                  {value}
                                </span>
                              </div>
                            )
                          )}
                        </div>
                      </ScrollArea>
                    </div>
                  )}
              </CardContent>
            </Card>
          )}

          {/* OS Context */}
          {contexts.os && (
            <Card>
              <CardHeader className="pb-3">
                <CardTitle className="text-sm font-medium">
                  Operating System
                </CardTitle>
              </CardHeader>
              <CardContent className="space-y-2">
                <div className="text-sm">
                  <div className="text-muted-foreground text-xs">Name</div>
                  <div>{contexts.os.name}</div>
                </div>
                {contexts.os.version && (
                  <div className="text-sm">
                    <div className="text-muted-foreground text-xs">Version</div>
                    <div className="font-mono">{contexts.os.version}</div>
                  </div>
                )}
                {contexts.os.kernel_version && (
                  <div className="text-sm">
                    <div className="text-muted-foreground text-xs">Kernel</div>
                    <div className="font-mono">
                      {contexts.os.kernel_version}
                    </div>
                  </div>
                )}
              </CardContent>
            </Card>
          )}

          {/* Device Context */}
          {contexts.device && (
            <Card>
              <CardHeader className="pb-3">
                <CardTitle className="text-sm font-medium">Device</CardTitle>
              </CardHeader>
              <CardContent className="space-y-2">
                {contexts.device.arch && (
                  <div className="text-sm">
                    <div className="text-muted-foreground text-xs">
                      Architecture
                    </div>
                    <div>{contexts.device.arch}</div>
                  </div>
                )}
                {contexts.device.cpu_description && (
                  <div className="text-sm">
                    <div className="text-muted-foreground text-xs">CPU</div>
                    <div className="text-xs">
                      {contexts.device.cpu_description}
                    </div>
                  </div>
                )}
                {contexts.device.processor_count && (
                  <div className="text-sm">
                    <div className="text-muted-foreground text-xs">Cores</div>
                    <div>{contexts.device.processor_count}</div>
                  </div>
                )}
                {contexts.device.memory_size && (
                  <div className="text-sm">
                    <div className="text-muted-foreground text-xs">Memory</div>
                    <div>
                      {(
                        contexts.device.memory_size /
                        1024 /
                        1024 /
                        1024
                      ).toFixed(1)}{' '}
                      GB
                    </div>
                  </div>
                )}
              </CardContent>
            </Card>
          )}

          {/* Runtime Context */}
          {contexts.runtime && (
            <Card>
              <CardHeader className="pb-3">
                <CardTitle className="text-sm font-medium">Runtime</CardTitle>
              </CardHeader>
              <CardContent className="space-y-2">
                <div className="text-sm">
                  <div className="text-muted-foreground text-xs">Name</div>
                  <div>{contexts.runtime.name}</div>
                </div>
                <div className="text-sm">
                  <div className="text-muted-foreground text-xs">Version</div>
                  <div className="font-mono">{contexts.runtime.version}</div>
                </div>
              </CardContent>
            </Card>
          )}

          {/* Response Context */}
          {contexts.response && (
            <Card>
              <CardHeader className="pb-3">
                <CardTitle className="text-sm font-medium">Response</CardTitle>
              </CardHeader>
              <CardContent className="space-y-2">
                {contexts.response.status_code && (
                  <div className="text-sm">
                    <div className="text-muted-foreground text-xs">
                      Status Code
                    </div>
                    <Badge
                      variant={
                        contexts.response.status_code >= 500
                          ? 'destructive'
                          : contexts.response.status_code >= 400
                            ? 'secondary'
                            : 'outline'
                      }
                    >
                      {contexts.response.status_code}
                    </Badge>
                  </div>
                )}
              </CardContent>
            </Card>
          )}

          {/* User Info */}
          {sentryData.user && (
            <Card>
              <CardHeader className="pb-3">
                <CardTitle className="text-sm font-medium">User</CardTitle>
              </CardHeader>
              <CardContent className="space-y-2">
                {sentryData.user.id && (
                  <div className="text-sm">
                    <div className="text-muted-foreground text-xs">ID</div>
                    <div className="font-mono">{sentryData.user.id}</div>
                  </div>
                )}
                {sentryData.user.email && (
                  <div className="text-sm">
                    <div className="text-muted-foreground text-xs">Email</div>
                    <div className="font-mono">{sentryData.user.email}</div>
                  </div>
                )}
                {sentryData.user.username && (
                  <div className="text-sm">
                    <div className="text-muted-foreground text-xs">
                      Username
                    </div>
                    <div className="font-mono">{sentryData.user.username}</div>
                  </div>
                )}
                {sentryData.user.ip_address && (
                  <div className="text-sm">
                    <div className="text-muted-foreground text-xs">
                      IP Address
                    </div>
                    <div className="font-mono">
                      {sentryData.user.ip_address}
                    </div>
                  </div>
                )}
              </CardContent>
            </Card>
          )}

          {/* OpenTelemetry Context */}
          {contexts.otel && (
            <Card>
              <CardHeader className="pb-3">
                <CardTitle className="text-sm font-medium">
                  OpenTelemetry
                </CardTitle>
              </CardHeader>
              <CardContent className="space-y-2">
                {contexts.otel.resource && (
                  <>
                    {contexts.otel.resource['service.name'] && (
                      <div className="text-sm">
                        <div className="text-muted-foreground text-xs">
                          Service Name
                        </div>
                        <div className="font-mono">
                          {contexts.otel.resource['service.name']}
                        </div>
                      </div>
                    )}
                    {contexts.otel.resource['service.version'] && (
                      <div className="text-sm">
                        <div className="text-muted-foreground text-xs">
                          Service Version
                        </div>
                        <div className="font-mono">
                          {contexts.otel.resource['service.version']}
                        </div>
                      </div>
                    )}
                    {contexts.otel.resource['telemetry.sdk.name'] && (
                      <div className="text-sm">
                        <div className="text-muted-foreground text-xs">
                          Telemetry SDK
                        </div>
                        <div className="font-mono">
                          {contexts.otel.resource['telemetry.sdk.name']} v
                          {contexts.otel.resource['telemetry.sdk.version']}
                        </div>
                      </div>
                    )}
                  </>
                )}
              </CardContent>
            </Card>
          )}

          {/* Trace Context with additional data */}
          {contexts.trace && (
            <Card>
              <CardHeader className="pb-3">
                <CardTitle className="text-sm font-medium">Trace</CardTitle>
              </CardHeader>
              <CardContent className="space-y-2">
                <div className="text-sm">
                  <div className="text-muted-foreground text-xs">Trace ID</div>
                  <div className="font-mono text-xs break-all">
                    {contexts.trace.trace_id}
                  </div>
                </div>
                <div className="text-sm">
                  <div className="text-muted-foreground text-xs">Span ID</div>
                  <div className="font-mono text-xs">
                    {contexts.trace.span_id}
                  </div>
                </div>
                {contexts.trace.parent_span_id && (
                  <div className="text-sm">
                    <div className="text-muted-foreground text-xs">
                      Parent Span ID
                    </div>
                    <div className="font-mono text-xs">
                      {contexts.trace.parent_span_id}
                    </div>
                  </div>
                )}
                {contexts.trace.op && (
                  <div className="text-sm">
                    <div className="text-muted-foreground text-xs">
                      Operation
                    </div>
                    <Badge variant="outline">{contexts.trace.op}</Badge>
                  </div>
                )}
                {contexts.trace.status && (
                  <div className="text-sm">
                    <div className="text-muted-foreground text-xs">Status</div>
                    <Badge
                      variant={
                        contexts.trace.status === 'ok'
                          ? 'outline'
                          : 'destructive'
                      }
                    >
                      {contexts.trace.status}
                    </Badge>
                  </div>
                )}
              </CardContent>
            </Card>
          )}

          {/* Tags */}
          {sentryData.tags && sentryData.tags.length > 0 && (
            <Card>
              <CardHeader className="pb-3">
                <CardTitle className="text-sm font-medium">Tags</CardTitle>
              </CardHeader>
              <CardContent>
                <div className="flex flex-wrap gap-2">
                  {sentryData.tags.map(([key, value], index) => (
                    <Badge key={index} variant="secondary">
                      {key}: {value}
                    </Badge>
                  ))}
                </div>
              </CardContent>
            </Card>
          )}

          {/* Measurements (Web Vitals & Performance) */}
          {sentryData.measurements &&
            Object.keys(sentryData.measurements).length > 0 && (
              <Card>
                <CardHeader className="pb-3">
                  <CardTitle className="text-sm font-medium">
                    Measurements
                  </CardTitle>
                  <CardDescription>
                    Performance metrics (
                    {Object.keys(sentryData.measurements).length})
                  </CardDescription>
                </CardHeader>
                <CardContent className="space-y-2">
                  {Object.entries(sentryData.measurements).map(
                    ([key, data]) => (
                      <div key={key} className="text-sm">
                        <div className="flex items-center justify-between">
                          <div className="text-muted-foreground text-xs">
                            {key}
                          </div>
                          <div className="font-mono text-xs">
                            {data.value.toFixed(2)}
                            {data.unit && ` ${data.unit}`}
                          </div>
                        </div>
                      </div>
                    )
                  )}
                </CardContent>
              </Card>
            )}

          {/* Fingerprint */}
          {sentryData.fingerprint && sentryData.fingerprint.length > 0 && (
            <Card>
              <CardHeader className="pb-3">
                <CardTitle className="text-sm font-medium">
                  Fingerprint
                </CardTitle>
                <CardDescription>Error grouping identifier</CardDescription>
              </CardHeader>
              <CardContent>
                <div className="space-y-1">
                  {sentryData.fingerprint.map((item, index) => (
                    <div key={index} className="font-mono text-xs break-all">
                      {item}
                    </div>
                  ))}
                </div>
              </CardContent>
            </Card>
          )}

          {/* Extra Data */}
          {sentryData.extra && Object.keys(sentryData.extra).length > 0 && (
            <Card>
              <CardHeader className="pb-3">
                <CardTitle className="text-sm font-medium">
                  Extra Data
                </CardTitle>
                <CardDescription>
                  Additional metadata ({Object.keys(sentryData.extra).length}{' '}
                  items)
                </CardDescription>
              </CardHeader>
              <CardContent>
                <ScrollArea className="h-[200px]">
                  <div className="space-y-2">
                    {Object.entries(sentryData.extra).map(([key, value]) => (
                      <div key={key} className="text-sm">
                        <div className="text-muted-foreground text-xs font-medium mb-1">
                          {key}
                        </div>
                        <div className="font-mono text-xs bg-muted p-2 rounded break-all">
                          {typeof value === 'object'
                            ? JSON.stringify(value, null, 2)
                            : String(value)}
                        </div>
                      </div>
                    ))}
                  </div>
                </ScrollArea>
              </CardContent>
            </Card>
          )}

          {/* Additional Contexts */}
          {contexts &&
            Object.keys(contexts).filter(
              (key) =>
                ![
                  'os',
                  'device',
                  'runtime',
                  'app',
                  'trace',
                  'culture',
                  'response',
                  'otel',
                  'cloud_resource',
                ].includes(key)
            ).length > 0 && (
              <Card>
                <CardHeader className="pb-3">
                  <CardTitle className="text-sm font-medium">
                    Additional Context
                  </CardTitle>
                </CardHeader>
                <CardContent>
                  <ScrollArea className="h-[200px]">
                    <pre className="text-xs">
                      {JSON.stringify(
                        Object.fromEntries(
                          Object.entries(contexts).filter(
                            ([key]) =>
                              ![
                                'os',
                                'device',
                                'runtime',
                                'app',
                                'trace',
                                'culture',
                              ].includes(key)
                          )
                        ),
                        null,
                        2
                      )}
                    </pre>
                  </ScrollArea>
                </CardContent>
              </Card>
            )}

          {/* Modules */}
          {sentryData.modules && Object.keys(sentryData.modules).length > 0 && (
            <Card>
              <CardHeader className="pb-3">
                <CardTitle className="text-sm font-medium">
                  Modules ({Object.keys(sentryData.modules).length})
                </CardTitle>
              </CardHeader>
              <CardContent>
                <ScrollArea className="h-[200px]">
                  <div className="space-y-1 text-xs">
                    {Object.entries(sentryData.modules).map(([key, value]) => (
                      <div key={key} className="flex gap-2">
                        <span className="font-mono font-medium">{key}:</span>
                        <span className="font-mono text-muted-foreground">
                          {value}
                        </span>
                      </div>
                    ))}
                  </div>
                </ScrollArea>
              </CardContent>
            </Card>
          )}
        </div>
      </div>
    </div>
  )
}
