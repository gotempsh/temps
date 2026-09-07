// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getPreviewGatewayLogs,
  getPreviewGatewaySettings,
  getPreviewGatewayStatus,
  patchPreviewGatewaySettings,
  restartPreviewGateway,
  upgradePreviewGateway,
  type GatewayStatus,
  type PreviewGatewaySettingsResponse,
} from '@/api/client'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'
import {
  AlertTriangle,
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  Globe,
  Loader2,
  RefreshCw,
  RotateCw,
  Save,
  X,
  XCircle,
} from 'lucide-react'
import { useCallback, useEffect, useState } from 'react'
import { toast } from 'sonner'

import {
  gatewayErrorAfterSuccessfulAction,
  previewGatewayErrorMessage,
  type GatewayAction,
  type GatewayActionError,
} from './preview-gateway-errors'

export function PreviewGatewayErrorAlert({
  error,
  onDismiss,
}: {
  error: GatewayActionError
  onDismiss: () => void
}) {
  return (
    <Alert variant="destructive" aria-live="assertive">
      <AlertTriangle className="h-4 w-4" />
      <AlertTitle>{error.title}</AlertTitle>
      <AlertDescription className="pr-8">{error.message}</AlertDescription>
      <Button
        variant="ghost"
        size="icon"
        className="absolute right-2 top-2 h-8 w-8"
        aria-label="Dismiss gateway error"
        onClick={onDismiss}
      >
        <X className="h-4 w-4" />
      </Button>
    </Alert>
  )
}

export function PreviewGatewayCard() {
  const [status, setStatus] = useState<GatewayStatus | null>(null)
  const [settings, setSettings] =
    useState<PreviewGatewaySettingsResponse | null>(null)
  const [loading, setLoading] = useState(true)
  const [imageInput, setImageInput] = useState('')
  const [hostPortInput, setHostPortInput] = useState('')
  const [autoUpgrade, setAutoUpgrade] = useState(true)
  const [isDirty, setIsDirty] = useState(false)
  const [busy, setBusy] = useState<null | 'restart' | 'upgrade' | 'save'>(null)
  const [logs, setLogs] = useState<string[] | null>(null)
  const [logsLoading, setLogsLoading] = useState(false)
  const [advancedOpen, setAdvancedOpen] = useState(false)
  const [actionError, setActionError] = useState<GatewayActionError | null>(
    null
  )

  const refresh = useCallback(async () => {
    try {
      const [statusResult, settingsResult] = await Promise.all([
        getPreviewGatewayStatus(),
        getPreviewGatewaySettings(),
      ])
      if (statusResult.data) setStatus(statusResult.data)
      if (settingsResult.data) {
        const s = settingsResult.data
        setSettings(s)
        setImageInput(s.image)
        setHostPortInput(String(s.host_port))
        setAutoUpgrade(s.auto_upgrade)
        setIsDirty(false)
      }

      const error = statusResult.error ?? settingsResult.error
      if (error !== undefined) {
        setActionError({
          action: 'refresh',
          title: 'Failed to refresh preview gateway',
          message: previewGatewayErrorMessage(
            error,
            'Gateway status or settings could not be loaded.',
            settingsResult.data?.host_port ?? statusResult.data?.host_port
          ),
        })
      } else {
        setActionError((current) =>
          gatewayErrorAfterSuccessfulAction(current, 'refresh')
        )
      }
    } catch (error) {
      setActionError({
        action: 'refresh',
        title: 'Failed to refresh preview gateway',
        message: previewGatewayErrorMessage(
          error,
          'Gateway status or settings could not be loaded.'
        ),
      })
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    // The initial request synchronizes this card with the external gateway
    // service; state updates happen only after the awaited HTTP responses.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    refresh()
  }, [refresh])

  const reportActionError = (
    action: GatewayAction,
    title: string,
    fallback: string,
    error: unknown
  ) => {
    const message = previewGatewayErrorMessage(
      error,
      fallback,
      settings?.host_port ?? status?.host_port
    )
    setActionError({ action, title, message })
    toast.error(title, { description: message })
  }

  const handleRestart = async () => {
    setBusy('restart')
    try {
      await restartPreviewGateway({ throwOnError: true })
      setActionError((current) =>
        gatewayErrorAfterSuccessfulAction(current, 'restart')
      )
      toast.success('Preview gateway restarted')
      await refresh()
    } catch (error) {
      reportActionError(
        'restart',
        'Failed to restart preview gateway',
        'The preview gateway could not be restarted.',
        error
      )
    } finally {
      setBusy(null)
    }
  }

  const handleUpgrade = async () => {
    setBusy('upgrade')
    try {
      await upgradePreviewGateway({
        body: { image: imageInput.trim() },
        throwOnError: true,
      })
      setActionError((current) =>
        gatewayErrorAfterSuccessfulAction(current, 'upgrade')
      )
      toast.success('Preview gateway upgraded')
      await refresh()
    } catch (error) {
      reportActionError(
        'upgrade',
        'Failed to upgrade preview gateway',
        'The preview gateway image could not be applied.',
        error
      )
    } finally {
      setBusy(null)
    }
  }

  const handleSaveSettings = async () => {
    const hostPort = Number(hostPortInput)
    if (!Number.isInteger(hostPort) || hostPort < 1 || hostPort > 65535) {
      const message = 'Host port must be a whole number between 1 and 65535.'
      setActionError({
        action: 'save',
        title: 'Invalid preview gateway settings',
        message,
      })
      toast.error('Invalid preview gateway settings', { description: message })
      return
    }

    setBusy('save')
    try {
      await patchPreviewGatewaySettings({
        body: {
          image: imageInput.trim() || undefined,
          host_port: hostPort,
          auto_upgrade: autoUpgrade,
        },
        throwOnError: true,
      })
      setActionError((current) =>
        gatewayErrorAfterSuccessfulAction(current, 'save')
      )
      toast.success('Settings saved')
      await refresh()
    } catch (error) {
      reportActionError(
        'save',
        'Failed to save gateway settings',
        'The preview gateway settings could not be saved.',
        error
      )
    } finally {
      setBusy(null)
    }
  }

  const handleResetImage = () => {
    if (settings) {
      setImageInput(settings.default_image)
      setIsDirty(true)
    }
  }

  const handleFetchLogs = async () => {
    setLogsLoading(true)
    try {
      const { data } = await getPreviewGatewayLogs({
        query: { tail: 200 },
        throwOnError: true,
      })
      setLogs(data.lines)
      setActionError((current) =>
        gatewayErrorAfterSuccessfulAction(current, 'logs')
      )
    } catch (error) {
      reportActionError(
        'logs',
        'Failed to fetch gateway logs',
        'Gateway logs could not be loaded.',
        error
      )
    } finally {
      setLogsLoading(false)
    }
  }

  if (loading) {
    return (
      <Card>
        <CardContent className="flex items-center justify-center py-8">
          <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
        </CardContent>
      </Card>
    )
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Globe className="h-5 w-5" />
          Workspace Preview Gateway
        </CardTitle>
        <CardDescription>
          Routes preview URLs (
          <code className="bg-muted px-1 rounded text-xs">
            ws-&lt;sandbox&gt;-&lt;port&gt;.preview-domain
          </code>
          ) to dev servers running inside agent sandboxes. A single shared
          Docker container per node — Temps manages it for you.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-6">
        {actionError && (
          <PreviewGatewayErrorAlert
            error={actionError}
            onDismiss={() => setActionError(null)}
          />
        )}

        {/* Status */}
        <div className="rounded-lg border p-4 space-y-3">
          <div className="flex items-center justify-between">
            <h4 className="text-sm font-medium">Status</h4>
            <Button
              variant="ghost"
              size="sm"
              onClick={refresh}
              aria-label="Refresh preview gateway status"
            >
              <RefreshCw className="h-3.5 w-3.5" />
            </Button>
          </div>
          {status ? (
            <div className="space-y-2">
              <div className="flex items-center gap-2 text-sm">
                {status.running ? (
                  <CheckCircle2 className="h-4 w-4 text-green-500" />
                ) : status.present ? (
                  <XCircle className="h-4 w-4 text-amber-500" />
                ) : (
                  <XCircle className="h-4 w-4 text-red-500" />
                )}
                <span>
                  {status.running
                    ? 'Running'
                    : status.present
                      ? 'Stopped'
                      : 'Not deployed'}
                </span>
                {status.drift && (
                  <span className="ml-2 inline-flex items-center gap-1 rounded bg-amber-500/10 px-2 py-0.5 text-xs text-amber-500">
                    <AlertTriangle className="h-3 w-3" />
                    Image drift
                  </span>
                )}
              </div>
              {status.image && (
                <p className="text-xs text-muted-foreground font-mono break-all">
                  Image: {status.image}
                </p>
              )}
              {status.network && (
                <p className="text-xs text-muted-foreground">
                  Network: {status.network}
                </p>
              )}
              {status.host_port != null && (
                <p className="text-xs text-muted-foreground">
                  Host port: 127.0.0.1:{status.host_port}
                </p>
              )}
              {status.restart_count != null && status.restart_count > 0 && (
                <p className="text-xs text-amber-500">
                  Docker has restarted this container {status.restart_count}{' '}
                  time(s)
                </p>
              )}
              <Button
                variant="outline"
                size="sm"
                onClick={handleRestart}
                disabled={busy !== null}
                className="mt-2"
              >
                {busy === 'restart' ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin mr-2" />
                ) : (
                  <RotateCw className="h-3.5 w-3.5 mr-2" />
                )}
                Restart
              </Button>
            </div>
          ) : (
            <p className="text-sm text-muted-foreground">
              Could not fetch gateway status.
            </p>
          )}
        </div>

        {/* Host port */}
        <div className="space-y-2">
          <Label htmlFor="gateway-host-port">Gateway host port</Label>
          <Input
            id="gateway-host-port"
            type="number"
            min={1}
            max={65535}
            value={hostPortInput}
            onChange={(event) => {
              setHostPortInput(event.target.value)
              setIsDirty(true)
            }}
            className="max-w-40 font-mono text-sm"
            aria-describedby="gateway-host-port-description"
          />
          <p
            id="gateway-host-port-description"
            className="text-sm text-muted-foreground"
          >
            Bound only on 127.0.0.1. If another process uses this port, choose a
            free port, save settings, then restart the gateway.
          </p>
        </div>

        {/* Image */}
        <div className="space-y-2">
          <Label htmlFor="gateway-image">Gateway image</Label>
          <div className="flex gap-2">
            <Input
              id="gateway-image"
              value={imageInput}
              onChange={(e) => {
                setImageInput(e.target.value)
                setIsDirty(true)
              }}
              placeholder={settings?.default_image}
              className="font-mono text-sm"
            />
            <Button
              variant="outline"
              size="sm"
              onClick={handleUpgrade}
              disabled={busy !== null || !imageInput.trim()}
              className="shrink-0"
            >
              {busy === 'upgrade' ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin mr-2" />
              ) : null}
              Pull & apply
            </Button>
          </div>
          {settings && imageInput !== settings.default_image && (
            <button
              onClick={handleResetImage}
              className="text-xs text-muted-foreground underline hover:text-foreground"
            >
              Reset to default ({settings.default_image})
            </button>
          )}
        </div>

        {/* Auto-upgrade */}
        <div className="flex items-center justify-between">
          <div className="space-y-0.5">
            <Label htmlFor="auto-upgrade">Auto-upgrade on restart</Label>
            <p className="text-sm text-muted-foreground">
              When enabled, Temps applies the configured image on every server
              restart. Disable for manual control.
            </p>
          </div>
          <Switch
            id="auto-upgrade"
            checked={autoUpgrade}
            onCheckedChange={(checked) => {
              setAutoUpgrade(checked)
              setIsDirty(true)
            }}
          />
        </div>

        {/* Save settings */}
        {isDirty && (
          <div className="flex justify-end">
            <Button
              size="sm"
              onClick={handleSaveSettings}
              disabled={busy !== null}
            >
              {busy === 'save' ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin mr-2" />
              ) : (
                <Save className="h-3.5 w-3.5 mr-2" />
              )}
              Save settings
            </Button>
          </div>
        )}

        {/* Advanced disclosure */}
        <div className="rounded-lg border">
          <button
            onClick={() => setAdvancedOpen(!advancedOpen)}
            className="w-full flex items-center justify-between p-3 text-sm font-medium hover:bg-muted/50 transition-colors"
          >
            <span className="flex items-center gap-2">
              {advancedOpen ? (
                <ChevronDown className="h-4 w-4" />
              ) : (
                <ChevronRight className="h-4 w-4" />
              )}
              Logs
            </span>
          </button>
          {advancedOpen && (
            <div className="border-t p-3 space-y-2">
              <Button
                variant="outline"
                size="sm"
                onClick={handleFetchLogs}
                disabled={logsLoading}
              >
                {logsLoading ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin mr-2" />
                ) : (
                  <RefreshCw className="h-3.5 w-3.5 mr-2" />
                )}
                Fetch last 200 lines
              </Button>
              {logs && logs.length > 0 && (
                <pre className="bg-muted rounded p-2 text-xs overflow-x-auto max-h-80 font-mono whitespace-pre-wrap">
                  {logs.join('\n')}
                </pre>
              )}
              {logs && logs.length === 0 && (
                <p className="text-xs text-muted-foreground">
                  No log output yet.
                </p>
              )}
            </div>
          )}
        </div>

        {!status?.present && (
          <Alert>
            <AlertTriangle className="h-4 w-4" />
            <AlertDescription className="text-sm">
              Gateway container is not deployed yet. It will be created
              automatically the next time the server starts, or click{' '}
              <strong>Pull &amp; apply</strong> above to deploy it now.
            </AlertDescription>
          </Alert>
        )}
      </CardContent>
    </Card>
  )
}
