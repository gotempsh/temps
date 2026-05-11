import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { useNavigate, useParams } from 'react-router-dom'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { useAuth } from '@/contexts/AuthContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { Check, X, AlertTriangle, Terminal } from 'lucide-react'

/**
 * Browser-side approval screen for the CLI device-authorization flow.
 *
 * The CLI prints `verification_uri_complete` (e.g. `/cli-login/ABCD-1234`)
 * and starts polling `/auth/cli/device/poll`. When the user lands here the
 * existing `ProtectedLayout` already enforces auth — if they're not signed
 * in they bounce through `/login` and `captureReturnTo()` brings them back
 * automatically. So this component only ever renders for authenticated
 * users; we just have to render device metadata + approve/deny.
 */
export function CliLogin() {
  usePageTitle('Authorize CLI')
  const { userCode: routeCode } = useParams<{ userCode?: string }>()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { user } = useAuth()

  // Allow `/cli-login` without a code by letting the user paste it. Most
  // traffic arrives at `/cli-login/:userCode`, but the route without a code
  // is reachable from the CLI as a fallback when auto-open fails.
  const [pastedCode, setPastedCode] = useState('')
  const userCode = routeCode?.trim().toUpperCase() ?? ''

  // These endpoints are too new to be in the regenerated SDK yet, so we
  // hand-roll fetch + the cookie-bearing client config. Once `openapi-ts`
  // runs against a server that has the device routes, this can move to the
  // generated mutation/query helpers like the rest of the codebase.
  const lookupQueryKey = ['cli-device-lookup', userCode] as const
  const lookup = useQuery({
    queryKey: lookupQueryKey,
    queryFn: () => fetchDeviceLookup(userCode),
    enabled: userCode.length > 0,
    refetchInterval: 5_000,
    refetchOnWindowFocus: false,
    retry: false,
    meta: { errorTitle: 'Could not load device session' },
  })

  const approve = useMutation({
    mutationFn: () => postDeviceAction('approve', userCode),
    meta: { errorTitle: 'Approval failed' },
    onSuccess: async () => {
      toast.success('CLI device authorized')
      await queryClient.invalidateQueries({ queryKey: lookupQueryKey })
    },
  })

  const deny = useMutation({
    mutationFn: () => postDeviceAction('deny', userCode),
    meta: { errorTitle: 'Denial failed' },
    onSuccess: async () => {
      toast.success('CLI device login denied')
      await queryClient.invalidateQueries({ queryKey: lookupQueryKey })
    },
  })

  // No code in the URL — show a small entry form.
  if (!userCode) {
    return (
      <div className="flex min-h-[60vh] items-center justify-center p-4">
        <Card className="w-full max-w-sm">
          <CardHeader>
            <div className="mb-2 flex h-10 w-10 items-center justify-center rounded-md bg-muted">
              <Terminal className="h-5 w-5" />
            </div>
            <CardTitle>Authorize CLI</CardTitle>
            <CardDescription>
              Enter the code shown in your terminal.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <form
              onSubmit={(e) => {
                e.preventDefault()
                const cleaned = pastedCode
                  .trim()
                  .toUpperCase()
                  .replace(/\s+/g, '')
                if (cleaned.length > 0) {
                  navigate(`/cli-login/${cleaned}`)
                }
              }}
            >
              <input
                autoFocus
                value={pastedCode}
                onChange={(e) => setPastedCode(e.target.value)}
                placeholder="ABCD-1234"
                className="w-full rounded-md border bg-background px-3 py-2 font-mono text-center text-lg tracking-widest uppercase outline-none focus:ring-2 focus:ring-ring"
                aria-label="Device code"
              />
            </form>
          </CardContent>
          <CardFooter>
            <Button
              type="button"
              className="w-full"
              disabled={pastedCode.trim().length === 0}
              onClick={() => {
                const cleaned = pastedCode
                  .trim()
                  .toUpperCase()
                  .replace(/\s+/g, '')
                navigate(`/cli-login/${cleaned}`)
              }}
            >
              Continue
            </Button>
          </CardFooter>
        </Card>
      </div>
    )
  }

  const status = lookup.data?.status
  const clientName = lookup.data?.client_name
  const requestedIp = lookup.data?.requested_ip
  const expiresAt = lookup.data?.expires_at

  const lookupErrorStatus = (lookup.error as { status?: number } | null)
    ?.status

  return (
    <div className="flex min-h-[60vh] items-center justify-center p-4">
      <Card className="w-full max-w-md">
        <CardHeader>
          <div className="mb-2 flex h-10 w-10 items-center justify-center rounded-md bg-muted">
            <Terminal className="h-5 w-5" />
          </div>
          <CardTitle>Authorize the Temps CLI?</CardTitle>
          <CardDescription>
            A device is requesting access to your account. Verify the details
            below match what's shown in your terminal.
          </CardDescription>
        </CardHeader>

        <CardContent className="space-y-4">
          <div className="rounded-md border bg-muted/30 p-3 text-center">
            <div className="text-xs uppercase tracking-wider text-muted-foreground">
              Code
            </div>
            <div className="font-mono text-2xl font-semibold tracking-widest">
              {userCode}
            </div>
          </div>

          {lookup.isLoading ? (
            <div className="space-y-2">
              <Skeleton className="h-4 w-3/4" />
              <Skeleton className="h-4 w-1/2" />
              <Skeleton className="h-4 w-2/3" />
            </div>
          ) : lookupErrorStatus === 404 ? (
            <ErrorRow
              title="Unknown code"
              detail="This code is not recognized. Check the code shown in your terminal."
            />
          ) : lookupErrorStatus === 410 || status === 'expired' ? (
            <ErrorRow
              title="Code expired"
              detail="Run the login command again in your terminal to get a fresh code."
            />
          ) : status === 'approved' ? (
            <StatusRow
              icon={<Check className="h-5 w-5 text-emerald-500" />}
              title="Authorized"
              detail="Your CLI is now signed in. You can close this tab."
            />
          ) : status === 'denied' ? (
            <StatusRow
              icon={<X className="h-5 w-5 text-destructive" />}
              title="Denied"
              detail="The CLI login request was denied."
            />
          ) : (
            <dl className="grid grid-cols-1 gap-2 text-sm">
              <Row label="Account" value={user?.email ?? '—'} mono />
              <Row label="Device" value={clientName ?? 'unknown'} mono />
              <Row label="From IP" value={requestedIp ?? 'unknown'} mono />
              <Row
                label="Expires"
                value={
                  expiresAt
                    ? new Date(expiresAt).toLocaleString()
                    : 'unknown'
                }
              />
            </dl>
          )}
        </CardContent>

        {status === 'pending' && (
          <CardFooter className="flex gap-2">
            <Button
              variant="outline"
              className="flex-1"
              disabled={deny.isPending || approve.isPending}
              onClick={() => deny.mutate()}
            >
              Deny
            </Button>
            <Button
              className="flex-1"
              disabled={deny.isPending || approve.isPending}
              onClick={() => approve.mutate()}
            >
              Authorize
            </Button>
          </CardFooter>
        )}
      </Card>
    </div>
  )
}

function Row({
  label,
  value,
  mono,
}: {
  label: string
  value: string
  mono?: boolean
}) {
  return (
    <div className="flex justify-between gap-2">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className={mono ? 'font-mono text-foreground' : 'text-foreground'}>
        {value}
      </dd>
    </div>
  )
}

function StatusRow({
  icon,
  title,
  detail,
}: {
  icon: React.ReactNode
  title: string
  detail: string
}) {
  return (
    <div className="flex items-start gap-3 rounded-md border p-3">
      <div className="mt-0.5">{icon}</div>
      <div className="space-y-0.5">
        <div className="text-sm font-medium">{title}</div>
        <div className="text-xs text-muted-foreground">{detail}</div>
      </div>
    </div>
  )
}

function ErrorRow({ title, detail }: { title: string; detail: string }) {
  return (
    <StatusRow
      icon={<AlertTriangle className="h-5 w-5 text-amber-500" />}
      title={title}
      detail={detail}
    />
  )
}

interface DeviceLookup {
  user_code: string
  status: 'pending' | 'approved' | 'denied' | 'expired'
  client_name: string | null
  requested_ip: string | null
  expires_at: string
}

/**
 * Wrapper around fetch that throws a `{status, title, detail}` shape so
 * react-query's `meta.errorTitle` and the rest of the UI can categorize
 * server failures the same way they do for the generated SDK.
 */
async function jsonRequest<T>(
  method: 'GET' | 'POST',
  path: string,
  body?: unknown,
): Promise<T> {
  const res = await fetch(path, {
    method,
    headers: { 'Content-Type': 'application/json' },
    credentials: 'include',
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  if (!res.ok) {
    let problem: { title?: string; detail?: string } | null = null
    try {
      problem = (await res.json()) as { title?: string; detail?: string }
    } catch {
      /* non-JSON body */
    }
    const err = new Error(problem?.detail || problem?.title || `Request failed (${res.status})`)
    ;(err as Error & { status?: number; title?: string; detail?: string }).status = res.status
    ;(err as Error & { status?: number; title?: string; detail?: string }).title = problem?.title
    ;(err as Error & { status?: number; title?: string; detail?: string }).detail = problem?.detail
    throw err
  }
  return (await res.json()) as T
}

async function fetchDeviceLookup(userCode: string): Promise<DeviceLookup> {
  return jsonRequest<DeviceLookup>(
    'GET',
    `/auth/cli/device/lookup?user_code=${encodeURIComponent(userCode)}`,
  )
}

async function postDeviceAction(
  action: 'approve' | 'deny',
  userCode: string,
): Promise<{ user_code: string; status: string }> {
  return jsonRequest('POST', `/auth/cli/device/${action}`, {
    user_code: userCode,
  })
}

// Re-export under both `default` and named so the lazy import in App.tsx
// matches either convention.
export default CliLogin
