// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { CodeTabs, type CodeExample } from '@/components/ui/code-tabs'
import { CopyButton } from '@/components/ui/copy-button'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { useSettings, useUpdateSettings } from '@/hooks/useSettings'
import { AlertTriangle, Bot, ExternalLink } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'

/**
 * MCP endpoints are mounted at the server root (not under /api), same as the
 * CLI's mcp command family -- see apps/temps-cli/src/commands/mcp/index.ts.
 * external_url is the operator-declared public address; fall back to the
 * origin this page itself was loaded from for local/dev instances that never
 * set it.
 */
function mcpBaseUrl(externalUrl: string | null | undefined): string {
  return externalUrl || window.location.origin
}

/** Mirrors CLIENT_ADAPTERS in apps/temps-cli/src/commands/mcp/clients/index.ts. */
const MCP_CLIENTS: { id: string; label: string }[] = [
  { id: 'claude-code', label: 'Claude Code' },
  { id: 'claude-desktop', label: 'Claude Desktop' },
  { id: 'codex', label: 'Codex' },
  { id: 'cursor', label: 'Cursor' },
  { id: 'vscode', label: 'VS Code' },
  { id: 'windsurf', label: 'Windsurf' },
  { id: 'zed', label: 'Zed' },
]

/**
 * Pins --url to this instance so the copied command is self-contained --
 * without it, `mcp add` falls back to whatever CLI context happens to be
 * active on the operator's machine, which silently may not be this instance
 * at all (e.g. they're also logged into a production server elsewhere).
 */
function connectExamples(baseUrl: string): CodeExample[] {
  return MCP_CLIENTS.map((c) => ({
    id: c.id,
    label: c.label,
    language: 'bash',
    code: `bunx @temps-sdk/cli mcp add ${c.id} --url ${baseUrl}`,
  }))
}

export function McpServerCard() {
  const { data: settings, isLoading, error } = useSettings()
  const updateSettings = useUpdateSettings()
  const [connectClientId, setConnectClientId] = useState(MCP_CLIENTS[0].id)

  if (isLoading) {
    return (
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Bot className="h-5 w-5" />
            MCP Server
          </CardTitle>
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
            <Bot className="h-5 w-5" />
            MCP Server
          </CardTitle>
        </CardHeader>
        <CardContent>
          <Alert variant="destructive">
            <AlertTriangle className="h-4 w-4" />
            <AlertTitle>Failed to load settings</AlertTitle>
            <AlertDescription>
              The server returned an error. Check console logs or contact your
              administrator.
            </AlertDescription>
          </Alert>
        </CardContent>
      </Card>
    )
  }

  const enabled = settings.mcp_server?.enabled ?? false
  const baseUrl = mcpBaseUrl(settings.external_url)

  const onCheckedChange = (checked: boolean) => {
    updateSettings.mutate(
      { mcp_server: { enabled: checked } },
      {
        onSuccess: () =>
          toast.success(
            checked ? 'MCP server enabled' : 'MCP server disabled'
          ),
      }
    )
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Bot className="h-5 w-5" />
          MCP Server
        </CardTitle>
        <CardDescription>
          Lets AI clients (Claude Code, Claude Desktop, Codex, Cursor, VS
          Code, Windsurf, Zed) connect to this Temps instance over the Model
          Context Protocol — e.g. ask "list my Temps projects" or "deploy the
          latest commit" and have the assistant call real tools against this
          instance. Every write action still requires a separate confirmation
          before it executes.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="flex items-start justify-between rounded-lg border p-3">
          <div className="space-y-0.5">
            <Label htmlFor="mcp-server-enabled" className="text-sm">
              Enable MCP server
            </Label>
            <p className="text-xs text-muted-foreground max-w-prose">
              Off by default — a fresh install never exposes the MCP endpoint
              until an admin turns it on here.
            </p>
          </div>
          <Switch
            id="mcp-server-enabled"
            checked={enabled}
            disabled={updateSettings.isPending}
            onCheckedChange={onCheckedChange}
          />
        </div>

        {enabled ? (
          <div className="space-y-3 rounded-lg border p-3">
            <div className="space-y-1">
              <Label className="text-sm">MCP endpoint</Label>
              <div className="flex items-center gap-2">
                <code className="flex-1 truncate rounded bg-muted px-2 py-1 text-xs">
                  {baseUrl}/mcp
                </code>
                <CopyButton
                  value={`${baseUrl}/mcp`}
                  minimal
                  label="Copy MCP endpoint"
                  className="shrink-0"
                />
              </div>
            </div>
            <div className="space-y-1">
              <Label className="text-sm">Connect a client</Label>
              <CodeTabs
                value={connectClientId}
                onValueChange={setConnectClientId}
                examples={connectExamples(baseUrl)}
                showLineNumbers={false}
              />
              <p className="text-xs text-muted-foreground">
                Runs an installer wizard that mints a scoped API key and
                writes the right config for {
                  MCP_CLIENTS.find((c) => c.id === connectClientId)?.label
                }. <code className="px-1 py-0.5 bg-muted rounded text-xs">--url</code>{' '}
                targets this instance directly, regardless of which CLI
                context is currently active on your machine.
              </p>
            </div>
            <a
              href="https://temps.sh/docs/set-up-mcp-locally"
              target="_blank"
              rel="noreferrer"
              className="inline-flex items-center gap-1 text-xs text-primary hover:underline"
            >
              Full setup guide
              <ExternalLink className="h-3 w-3" />
            </a>
          </div>
        ) : (
          <Alert>
            <Bot className="h-4 w-4" />
            <AlertTitle>MCP is off</AlertTitle>
            <AlertDescription>
              Turn it on above, then run{' '}
              <code className="px-1 py-0.5 bg-muted rounded text-xs">
                bunx @temps-sdk/cli mcp add --url {baseUrl}
              </code>{' '}
              to connect an AI client.
            </AlertDescription>
          </Alert>
        )}
      </CardContent>
    </Card>
  )
}
