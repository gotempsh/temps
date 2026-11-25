// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { AiHarnessLogo } from '@/components/ui/ai-harness-logo'
import { CopyButton } from '@/components/ui/copy-button'
import { extensionCanConnect } from '@/lib/claude-extension/extension'
import {
  MCP_APPS,
  type McpAppId,
  mcpAddCommand,
  mcpApp,
  mcpAppSnippet,
} from '@/lib/mcp-clients'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Callout } from '@temps-sdk/ds'
import { useState } from 'react'
import { mcpSetupBlocker } from './agents'
import { McpAccessFooter, McpKeyStep, McpServerStep } from './McpAccessSteps'
import { useMcpAccess } from './useMcpAccess'

/**
 * Another app path: Cursor, VS Code, Windsurf and Zed speak MCP over HTTP
 * and run on this computer, so they connect to this console's MCP endpoint
 * themselves, with no extension. The user turns on the MCP server, creates
 * a key, and pastes a settings snippet that already holds it.
 */
export function OtherAppSetup() {
  const origin = window.location.origin
  const [appId, setAppId] = useState<McpAppId>('cursor')
  const app = mcpApp(appId)
  const mcp = useMcpAccess(appId)
  const blocker = mcpSetupBlocker(
    mcp.mcpState,
    mcp.apiKey !== null,
    'copy the settings'
  )
  const snippet = mcp.apiKey ? mcpAppSnippet(appId, origin, mcp.apiKey) : null
  const command = mcpAddCommand(appId, origin, mcp.access)
  const deployCommand = mcpAddCommand(appId, origin, 'changes', '~/code')

  return (
    <div className="space-y-4">
      <div className="space-y-1.5">
        <Label htmlFor="other-app">Which app?</Label>
        <Select
          value={appId}
          onValueChange={(value) => setAppId(value as McpAppId)}
        >
          <SelectTrigger id="other-app" className="w-full sm:w-[240px]">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {MCP_APPS.map((app) => (
              <SelectItem key={app.id} value={app.id}>
                <span className="flex items-center gap-2">
                  <AiHarnessLogo providerId={app.id} size={16} />
                  {app.name}
                </span>
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      {!extensionCanConnect(origin) && (
        <Callout tone="warning" title="The key would travel unencrypted">
          {app.name} connects to this address over plain HTTP. Open this console
          over HTTPS or through Tailscale before you add it.
        </Callout>
      )}

      <ol className="space-y-4 text-sm">
        <McpServerStep mcp={mcp} />
        <McpKeyStep mcp={mcp} pasteHint="It is in the settings below." />

        <li className="space-y-2">
          <p className="font-medium">3. Add Temps to {app.name}</p>
          {blocker || !snippet ? (
            <p className="text-muted-foreground" role="status">
              {blocker}
            </p>
          ) : (
            <div className="space-y-2">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <p className="text-muted-foreground">
                  Merge this into{' '}
                  <code className="font-mono [overflow-wrap:anywhere]">
                    {app.settingsFile}
                  </code>
                  .
                </p>
                <CopyButton
                  value={snippet}
                  minimal
                  label="Copy settings"
                  className="h-8 rounded-md border px-3"
                >
                  Copy settings
                </CopyButton>
              </div>
              <pre
                role="region"
                aria-label={`${app.name} settings`}
                tabIndex={0}
                className="max-h-56 overflow-y-auto whitespace-pre-wrap rounded-md border bg-muted/40 p-3 font-mono text-xs [overflow-wrap:anywhere]"
              >
                {snippet}
              </pre>
              <p className="text-xs text-muted-foreground">
                The file holds the key in plain text. Keep it to yourself.
              </p>
            </div>
          )}
          <p className="text-muted-foreground">
            Or let the Temps CLI write it, with its own login:
          </p>
          <div className="flex items-center gap-2 rounded-md border bg-muted/40 px-3 py-2">
            <code className="min-w-0 flex-1 font-mono text-xs [overflow-wrap:anywhere]">
              {command}
            </code>
            <CopyButton value={command} />
          </div>
          <p className="text-muted-foreground">
            To deploy project folders from this computer too, use this instead,
            with your projects’ folder in place of ~/code.
          </p>
          <div className="flex items-center gap-2 rounded-md border bg-muted/40 px-3 py-2">
            <code className="min-w-0 flex-1 font-mono text-xs [overflow-wrap:anywhere]">
              {deployCommand}
            </code>
            <CopyButton value={deployCommand} />
          </div>
        </li>

        <li className="space-y-2">
          <p className="font-medium">4. Try it</p>
          <p className="text-muted-foreground">
            Reload {app.name}’s MCP servers, then ask “What’s running on my
            Temps server?”. Changes wait for your approval.
          </p>
        </li>
      </ol>

      <McpAccessFooter mcp={mcp} />
    </div>
  )
}
