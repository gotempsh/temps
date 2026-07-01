import { useAuth } from '@/contexts/AuthContext'
import { captureReturnTo } from '@/lib/return-to'
import { Login } from '@/pages/Login'
import { useConsoleExtensions } from '@temps-sdk/console-kit'
import {
  AlertCircle,
  RefreshCw,
  ServerCrash,
  WifiOff,
  Clock,
  ShieldAlert,
  HelpCircle,
  ExternalLink,
  Terminal,
  Copy,
  Check,
} from 'lucide-react'
import { useState } from 'react'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import type { LucideIcon } from 'lucide-react'

type ErrorCategory = 'network' | 'timeout' | 'forbidden' | 'server'

interface CategorizedError {
  category: ErrorCategory
  icon: LucideIcon
  title: string
  summary: string
  status?: number
  statusLabel?: string
  troubleshooting: string[]
}

function categorizeError(error: unknown): CategorizedError {
  const errorObj = error as {
    title?: string
    detail?: string
    message?: string
    status?: number
    code?: string
  } | null
  const errorTitle = errorObj?.title ?? ''
  const errorMessage = errorObj?.message ?? ''
  const status = errorObj?.status
  const code = errorObj?.code

  const isNetwork =
    errorMessage.includes('Failed to fetch') ||
    errorMessage.includes('Network') ||
    errorMessage.includes('NetworkError') ||
    code === 'ECONNREFUSED' ||
    code === 'ERR_NETWORK'

  if (isNetwork) {
    return {
      category: 'network',
      icon: WifiOff,
      title: 'Cannot reach the server',
      summary:
        'Your browser could not connect to the Temps API. The server may be offline or unreachable from your network.',
      statusLabel: 'Network error',
      troubleshooting: [
        'Check your internet connection and VPN, if any.',
        'Verify the Temps service is running: `systemctl status temps` or `docker ps`.',
        'If self-hosting, confirm `TEMPS_ADDRESS` is bound to the correct interface.',
        'Inspect browser devtools → Network tab for the failing request details.',
      ],
    }
  }

  if (errorTitle === 'Gateway Timeout' || status === 504) {
    return {
      category: 'timeout',
      icon: Clock,
      title: 'The server took too long to respond',
      summary:
        'Requests to the API are timing out. The server may be overloaded or a downstream service (database, proxy) is unresponsive.',
      status: 504,
      statusLabel: '504 Gateway Timeout',
      troubleshooting: [
        'Wait a few seconds and retry — this is often transient.',
        'Check server load and memory: high CPU or swap usage will slow requests.',
        'Verify the database is reachable and not under heavy load.',
        'Inspect server logs for slow queries or blocked tasks.',
      ],
    }
  }

  if (status === 403 || errorTitle === 'Forbidden') {
    return {
      category: 'forbidden',
      icon: ShieldAlert,
      title: 'You do not have access to this resource',
      summary:
        'Your account is authenticated, but lacks permission for this page. An administrator must grant access.',
      status: 403,
      statusLabel: '403 Forbidden',
      troubleshooting: [
        'Contact your workspace administrator to request the required role.',
        'Sign out and sign back in if your permissions were recently changed.',
      ],
    }
  }

  const status5xx =
    typeof status === 'number' && status >= 500 && status < 600 ? status : 500
  return {
    category: 'server',
    icon: ServerCrash,
    title: 'The server encountered an error',
    summary:
      'The API returned an unexpected error. This is usually temporary, but persistent failures often indicate a misconfiguration or outage.',
    status: status5xx,
    statusLabel: `${status5xx} ${status5xx === 503 ? 'Service Unavailable' : 'Internal Server Error'}`,
    troubleshooting: [
      'Retry in a few seconds — many 5xx errors resolve on their own.',
      'Check server logs: `journalctl -u temps -f` or the Temps log output.',
      'Confirm database connectivity (`TEMPS_DATABASE_URL`) and migrations are up to date.',
      'Open the status page or contact support if the issue persists.',
    ],
  }
}

function ErrorDetails({
  error,
  message,
}: {
  error: unknown
  message: string
}) {
  const [copied, setCopied] = useState(false)
  const payload = (() => {
    try {
      return JSON.stringify(error, null, 2)
    } catch {
      return message
    }
  })()

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(payload)
      setCopied(true)
      setTimeout(() => setCopied(false), 2000)
    } catch {
      /* no-op */
    }
  }

  return (
    <details className="group rounded-md border bg-muted/30">
      <summary className="flex cursor-pointer items-center justify-between gap-2 px-4 py-2.5 text-sm font-medium hover:bg-muted/50">
        <span className="flex items-center gap-2">
          <Terminal className="h-4 w-4 text-muted-foreground" />
          Technical details
        </span>
        <span className="text-xs text-muted-foreground group-open:hidden">
          Expand
        </span>
        <span className="hidden text-xs text-muted-foreground group-open:inline">
          Collapse
        </span>
      </summary>
      <div className="space-y-2 border-t p-3">
        <div className="flex items-center justify-between">
          <p className="text-xs font-medium text-muted-foreground">
            Error payload
          </p>
          <Button
            size="sm"
            variant="ghost"
            className="h-7 gap-1.5 text-xs"
            onClick={handleCopy}
          >
            {copied ? (
              <>
                <Check className="h-3 w-3" /> Copied
              </>
            ) : (
              <>
                <Copy className="h-3 w-3" /> Copy
              </>
            )}
          </Button>
        </div>
        <pre className="max-h-48 overflow-auto whitespace-pre-wrap break-all rounded bg-background p-3 font-mono text-xs">
          {payload}
        </pre>
      </div>
    </details>
  )
}

export const ProtectedLayout = ({
  children,
}: {
  children: React.ReactNode
}) => {
  const { user, isLoading, error, refetch } = useAuth()
  // Allow an EE-style extension to replace the OSS login screen wholesale
  // (e.g. swap the email/password form for an SSO-only view when the EE
  // password-login policy is enabled). Falls back to <Login /> when no
  // extension provides one — preserving OSS behaviour unchanged.
  const { loginPage } = useConsoleExtensions()
  const loginElement = loginPage ?? <Login />

  if (isLoading) {
    return (
      <div className="flex items-center justify-center h-dvh">
        <div className="text-center space-y-2">
          <div className="h-8 w-8 animate-spin rounded-full border-4 border-primary border-t-transparent mx-auto" />
          <p className="text-sm text-muted-foreground">Loading...</p>
        </div>
      </div>
    )
  }

  if (error) {
    const errorObj = error as { title?: string; detail?: string } | null
    const errorTitle = errorObj?.title
    const errorDetail = errorObj?.detail
    const errorMessage =
      error?.message || errorDetail || 'An unexpected error occurred'

    if (
      errorTitle === 'Authentication Required' ||
      errorTitle === 'Unauthorized'
    ) {
      captureReturnTo()
      return loginElement
    }

    const categorized = categorizeError(error)
    const Icon = categorized.icon
    const retryLabel =
      categorized.category === 'network' ? 'Retry connection' : 'Try again'

    return (
      <div className="flex min-h-dvh items-center justify-center bg-muted/20 p-4">
        <Card className="w-full max-w-xl shadow-sm">
          <CardHeader className="space-y-4">
            <div className="flex items-start gap-4">
              <div className="flex h-12 w-12 shrink-0 items-center justify-center rounded-full bg-destructive/10">
                <Icon className="h-6 w-6 text-destructive" />
              </div>
              <div className="flex-1 space-y-1.5">
                <div className="flex flex-wrap items-center gap-2">
                  <CardTitle className="text-xl">{categorized.title}</CardTitle>
                  {categorized.statusLabel && (
                    <Badge variant="outline" className="font-mono text-xs">
                      {categorized.statusLabel}
                    </Badge>
                  )}
                </div>
                <CardDescription className="text-sm leading-relaxed">
                  {categorized.summary}
                </CardDescription>
              </div>
            </div>
          </CardHeader>
          <CardContent className="space-y-5">
            <div className="rounded-md border border-destructive/30 bg-destructive/5 p-3">
              <div className="flex items-start gap-2">
                <AlertCircle className="mt-0.5 h-4 w-4 shrink-0 text-destructive" />
                <div className="min-w-0 flex-1 space-y-0.5">
                  <p className="text-xs font-medium text-destructive">
                    {errorTitle || 'Error message'}
                  </p>
                  <p className="break-words font-mono text-xs text-destructive/90">
                    {errorMessage}
                  </p>
                </div>
              </div>
            </div>

            <div className="space-y-2.5">
              <div className="flex items-center gap-2">
                <HelpCircle className="h-4 w-4 text-muted-foreground" />
                <h4 className="text-sm font-medium">Troubleshooting steps</h4>
              </div>
              <ol className="ml-1 space-y-1.5 text-sm text-muted-foreground">
                {categorized.troubleshooting.map((step, i) => (
                  <li key={i} className="flex gap-3">
                    <span className="mt-0.5 flex h-5 w-5 shrink-0 items-center justify-center rounded-full bg-muted text-xs font-medium text-foreground">
                      {i + 1}
                    </span>
                    <span className="leading-relaxed">{step}</span>
                  </li>
                ))}
              </ol>
            </div>

            <ErrorDetails error={error} message={errorMessage} />

            <div className="flex flex-col gap-2 sm:flex-row">
              <Button
                onClick={() => refetch()}
                className="flex-1"
                variant="default"
              >
                <RefreshCw className="mr-2 h-4 w-4" />
                {retryLabel}
              </Button>
              <Button
                onClick={() => window.location.reload()}
                variant="outline"
                className="flex-1"
              >
                Reload page
              </Button>
            </div>

            <div className="flex flex-wrap items-center justify-between gap-2 border-t pt-4 text-xs text-muted-foreground">
              <span>
                Still not working? Check the{' '}
                <a
                  href="https://docs.temps.sh"
                  target="_blank"
                  rel="noreferrer"
                  className="inline-flex items-center gap-1 font-medium text-foreground underline-offset-4 hover:underline"
                >
                  documentation
                  <ExternalLink className="h-3 w-3" />
                </a>
                .
              </span>
              <a
                href="https://github.com/temps-sh/temps/issues"
                target="_blank"
                rel="noreferrer"
                className="inline-flex items-center gap-1 font-medium text-foreground underline-offset-4 hover:underline"
              >
                Report an issue
                <ExternalLink className="h-3 w-3" />
              </a>
            </div>
          </CardContent>
        </Card>
      </div>
    )
  }

  if (!user) {
    captureReturnTo()
    return loginElement
  }

  return <>{children}</>
}
