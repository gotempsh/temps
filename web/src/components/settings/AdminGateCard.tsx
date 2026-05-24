import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
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
import { Textarea } from '@/components/ui/textarea'
import {
  getAdminGateOptions,
  patchAdminGateMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { AlertTriangle, Loader2, Lock, Save, Shield } from 'lucide-react'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'

/**
 * Admin Gate management card.
 *
 * The admin gate is a defense-in-depth IP/host allowlist enforced *in front
 * of* the admin listener. This card lets a SettingsWrite user manage the
 * lists at runtime — but only when env vars haven't pinned the config.
 * When `TEMPS_ADMIN_ALLOWED_IPS` (or its siblings) is set, the backend
 * returns `editable: false` and the UI renders read-only with a banner
 * explaining why.
 */
export function AdminGateCard() {
  const queryClient = useQueryClient()
  const { data, isLoading, error } = useQuery(getAdminGateOptions())

  const [allowedIpsText, setAllowedIpsText] = useState('')
  const [allowedHostsText, setAllowedHostsText] = useState('')
  const [trustForwardedFor, setTrustForwardedFor] = useState(false)
  const [dirty, setDirty] = useState(false)

  useEffect(() => {
    if (!data) return
    setAllowedIpsText((data.allowed_ips ?? []).join('\n'))
    setAllowedHostsText((data.allowed_hosts ?? []).join('\n'))
    setTrustForwardedFor(Boolean(data.trust_forwarded_for))
    setDirty(false)
  }, [data])

  const updateMutation = useMutation({
    ...patchAdminGateMutation(),
    onSuccess: (next) => {
      queryClient.setQueryData(getAdminGateOptions().queryKey, next)
      setDirty(false)
      toast.success('Admin gate updated')
    },
    onError: (err) => {
      const maybeDetail =
        err && typeof err === 'object' && 'detail' in err
          ? (err as { detail?: unknown }).detail
          : undefined
      toast.error(
        typeof maybeDetail === 'string'
          ? maybeDetail
          : 'Failed to update admin gate'
      )
    },
  })

  if (isLoading) {
    return (
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Shield className="h-5 w-5" />
            Admin Access Gate
          </CardTitle>
          <CardDescription>
            Restrict which IPs and hostnames can reach the admin console.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <Skeleton className="h-24 w-full" />
          <Skeleton className="h-24 w-full" />
          <Skeleton className="h-8 w-1/2" />
        </CardContent>
      </Card>
    )
  }

  if (error || !data) {
    return (
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Shield className="h-5 w-5" />
            Admin Access Gate
          </CardTitle>
        </CardHeader>
        <CardContent>
          <Alert variant="destructive">
            <AlertTriangle className="h-4 w-4" />
            <AlertTitle>Failed to load admin gate settings</AlertTitle>
            <AlertDescription>
              The server returned an error. Check console logs or contact your
              administrator.
            </AlertDescription>
          </Alert>
        </CardContent>
      </Card>
    )
  }

  const editable = data.editable === true
  const envOverride = data.source === 'env'

  const onSubmit = () => {
    const allowed_ips = allowedIpsText
      .split(/\r?\n/)
      .map((s) => s.trim())
      .filter(Boolean)
    const allowed_hosts = allowedHostsText
      .split(/\r?\n/)
      .map((s) => s.trim())
      .filter(Boolean)
    updateMutation.mutate({
      body: {
        allowed_ips,
        allowed_hosts,
        trust_forwarded_for: trustForwardedFor,
      },
    })
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Shield className="h-5 w-5" />
          Admin Access Gate
        </CardTitle>
        <CardDescription>
          Restrict which hostnames and source IPs can reach the management
          surface (dashboard, <code>/api/projects</code>, etc.) through the
          public load balancer. Hosts that don't match are served as normal
          LB traffic — if they resolve to a deployed app it serves; otherwise
          they 404. Public ingest (<code>/api/_temps/*</code>) is always
          reachable from any host. Empty lists = no restriction. Bare IPs are
          accepted as /32 (or /128 for IPv6). CIDR allowed for ranges.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {envOverride && (
          <Alert>
            <Lock className="h-4 w-4" />
            <AlertTitle>Managed by environment variables</AlertTitle>
            <AlertDescription>
              <code>TEMPS_ADMIN_ALLOWED_IPS</code> /{' '}
              <code>TEMPS_ADMIN_ALLOWED_HOSTS</code> /{' '}
              <code>TEMPS_ADMIN_TRUST_FORWARDED_FOR</code> are set on the
              process — the lists below are read-only. Unset those env vars
              and restart the server to manage the gate from this page.
            </AlertDescription>
          </Alert>
        )}

        <div className="space-y-2">
          <Label htmlFor="admin-gate-ips">Allowed IPs / CIDRs</Label>
          <Textarea
            id="admin-gate-ips"
            placeholder={'10.0.0.0/8\n203.0.113.5\n2001:db8::/32'}
            rows={4}
            value={allowedIpsText}
            disabled={!editable}
            onChange={(e) => {
              setAllowedIpsText(e.target.value)
              setDirty(true)
            }}
          />
          <p className="text-xs text-muted-foreground">
            One entry per line. Leave empty to allow any source.
          </p>
        </div>

        <div className="space-y-2">
          <Label htmlFor="admin-gate-hosts">Allowed Host headers</Label>
          <Textarea
            id="admin-gate-hosts"
            placeholder={'admin.example.com\nconsole.internal'}
            rows={3}
            value={allowedHostsText}
            disabled={!editable}
            onChange={(e) => {
              setAllowedHostsText(e.target.value)
              setDirty(true)
            }}
          />
          <p className="text-xs text-muted-foreground">
            One hostname per line. Port is stripped. Leave empty to accept any
            Host header.
          </p>
        </div>

        <div className="flex items-start justify-between rounded-lg border p-3">
          <div className="space-y-0.5">
            <Label htmlFor="admin-gate-xff" className="text-sm">
              Trust <code className="text-xs">X-Forwarded-For</code> from
              loopback
            </Label>
            <p className="text-xs text-muted-foreground max-w-prose">
              When enabled, the gate uses the leftmost XFF entry as the client
              IP — but only when the immediate peer is loopback (127.0.0.0/8 or
              ::1). Required when the admin listener sits behind a local
              reverse proxy. External clients cannot spoof XFF.
            </p>
          </div>
          <Switch
            id="admin-gate-xff"
            checked={trustForwardedFor}
            disabled={!editable}
            onCheckedChange={(checked) => {
              setTrustForwardedFor(checked)
              setDirty(true)
            }}
          />
        </div>

        {editable && dirty && (
          <div className="flex justify-end pt-2">
            <Button
              type="button"
              onClick={onSubmit}
              disabled={updateMutation.isPending}
              size="sm"
            >
              {updateMutation.isPending ? (
                <>
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  Saving...
                </>
              ) : (
                <>
                  <Save className="mr-2 h-4 w-4" />
                  Save
                </>
              )}
            </Button>
          </div>
        )}
      </CardContent>
    </Card>
  )
}
