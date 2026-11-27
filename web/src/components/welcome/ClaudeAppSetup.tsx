// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  CLAUDE_EXTENSION_FILENAME,
  buildClaudeExtension,
  extensionCanConnect,
} from '@/lib/claude-extension/extension'
import { Button, Callout } from '@temps-sdk/ds'
import { useMutation } from '@tanstack/react-query'
import { toast } from 'sonner'
import { mcpSetupBlocker } from './agents'
import { McpAccessFooter, McpKeyStep, McpServerStep } from './McpAccessSteps'
import { useMcpAccess } from './useMcpAccess'

/** Build the extension for this console and hand it to the browser as a download. */
async function downloadExtension(origin: string): Promise<void> {
  // Loaded on demand: only people installing the extension need its server.
  const [{ default: bridge }, { default: deploy }] = await Promise.all([
    import('@/lib/claude-extension/bridge.cjs?raw'),
    import('@/lib/claude-extension/deploy.cjs?raw'),
  ])
  const bytes = buildClaudeExtension(origin, { bridge, deploy })
  const url = URL.createObjectURL(
    new Blob([bytes as BlobPart], { type: 'application/zip' })
  )
  try {
    const link = document.createElement('a')
    link.href = url
    link.download = CLAUDE_EXTENSION_FILENAME
    link.click()
  } finally {
    // Revoked on the next task, after the click has started the download.
    setTimeout(() => URL.revokeObjectURL(url), 0)
  }
}

/**
 * The Claude app path: Claude runs the Temps extension on this computer and
 * it talks to this console's MCP endpoint, so it works where the app's own
 * shell cannot reach (localhost, a tailnet). The user turns on the MCP
 * server, creates a key, installs the extension and tries it. Cowork and
 * Code sessions don't run extensions; `onUseCodingAgent` switches to the
 * coding-agent prompt, which they can use instead.
 */
export function ClaudeAppSetup({
  onUseCodingAgent,
}: {
  onUseCodingAgent?: () => void
}) {
  const origin = window.location.origin
  const mcp = useMcpAccess('claude-app')

  const download = useMutation({
    mutationFn: () => downloadExtension(origin),
    onSuccess: () => toast.success(`Downloaded ${CLAUDE_EXTENSION_FILENAME}`),
    onError: (error) =>
      toast.error(
        error instanceof Error ? error.message : 'Could not build the extension'
      ),
  })

  const downloadBlocker = mcpSetupBlocker(
    mcp.mcpState,
    mcp.apiKey !== null,
    'download the extension'
  )
  const canConnect = extensionCanConnect(origin)

  return (
    <div className="space-y-4">
      {!canConnect && (
        <Callout
          tone="warning"
          title="The extension won't connect to this address"
        >
          It sends your key only over HTTPS, or plain HTTP to this computer or a
          Tailscale address. Open this console over HTTPS or through Tailscale
          first.
        </Callout>
      )}

      <ol className="space-y-4 text-sm">
        <McpServerStep mcp={mcp} />
        <McpKeyStep
          mcp={mcp}
          pasteHint="Paste it when Claude asks for the API key."
        />

        <li className="space-y-2">
          <p className="font-medium">3. Install the extension in Claude</p>
          <Button
            variant="outline"
            busy={download.isPending}
            busyLabel="Building…"
            aria-disabled={downloadBlocker !== null || undefined}
            aria-describedby={
              downloadBlocker ? 'claude-app-download-blocker' : undefined
            }
            onClick={() => !downloadBlocker && download.mutate()}
          >
            Download {CLAUDE_EXTENSION_FILENAME}
          </Button>
          {downloadBlocker && (
            <p
              id="claude-app-download-blocker"
              className="text-muted-foreground"
              role="status"
            >
              {downloadBlocker}
            </p>
          )}
          <ul
            className="list-disc space-y-1 pl-5 text-muted-foreground"
            aria-label="Install the extension"
          >
            <li>
              Open{' '}
              <code className="font-mono">{CLAUDE_EXTENSION_FILENAME}</code>. If
              no install page appears, drag it onto Claude’s Settings, then
              Extensions.
            </li>
            <li>
              Click Install. The “not signed” warning is expected: this console
              built the file for this address.
            </li>
            <li>
              Keep the server address,{' '}
              <code className="font-mono [overflow-wrap:anywhere]">
                {origin}
              </code>
              , and paste the key from step 2 as the API key.
            </li>
            <li>
              To deploy from this computer, add your projects’ folder under
              “Folders Claude may deploy”. It never uploads .env files, keys or
              what .gitignore lists.
            </li>
          </ul>
        </li>

        <li className="space-y-2">
          <p className="font-medium">4. Try it</p>
          <p className="text-muted-foreground">
            In a new chat, ask “What’s running on my Temps server?” or “Deploy
            ~/code/my-app to Temps”. Changes wait for your approval.
          </p>
          <p className="text-muted-foreground">
            Cowork and Code sessions don’t run extensions. There, the
            coding-agent prompt signs in with the Temps CLI and deploys with
            temps drop, if the session can reach Temps over public HTTPS.{' '}
            {onUseCodingAgent && (
              <Button
                variant="link"
                className="h-auto p-0"
                onClick={onUseCodingAgent}
              >
                Show the coding-agent prompt
              </Button>
            )}
          </p>
        </li>
      </ol>

      <McpAccessFooter mcp={mcp} />
    </div>
  )
}
