// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { ChevronDown, ChevronRight, Terminal } from 'lucide-react'

import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { CodeBlock } from '@/components/ui/code-block'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'

// Inline docs for creating sandboxes. Lives on the /sandboxes page because
// we don't yet have a "New sandbox" UI flow — users go through the CLI,
// REST, or the SDK. Keep the three examples behaviorally identical so a
// reader can pick whichever fits their workflow and expect the same result.

export const SANDBOX_CLI_EXAMPLE = `# Authenticate once (cached in ~/.temps/.contexts.json)
bunx @temps-sdk/cli login https://your-temps-instance.com --context my-instance

# Create a sandbox with a 2h idle timeout
bunx @temps-sdk/cli --target-context my-instance sandbox create --timeout-secs 7200 --name my-sandbox

# List, show details, or exec a command inside it
bunx @temps-sdk/cli --target-context my-instance sandbox list
bunx @temps-sdk/cli --target-context my-instance sandbox show sbx_abc123
bunx @temps-sdk/cli --target-context my-instance sandbox exec sbx_abc123 -- node --version

# Remove the sandbox when done (aliases: \`stop\`, \`destroy\`)
bunx @temps-sdk/cli --target-context my-instance sandbox rm sbx_abc123`

export const SANDBOX_WORKSPACE_EXAMPLE = `# Use the named context created in the CLI tab. From a linked checkout,
# the project comes from .temps/config.json, the repo and branch from your git remote.
cd ~/code/my-repo
bunx @temps-sdk/cli --target-context my-instance sandbox create --workspace

# Name the project explicitly, and start a fresh branch
bunx @temps-sdk/cli --target-context my-instance sandbox create --workspace \\
  --project my-api --branch main --new-branch feat/checkout

# A repo that has no temps project — resolved off your git connection,
# so private repos clone without you handling a token
bunx @temps-sdk/cli --target-context my-instance sandbox create --workspace --repo acme/internal-tools

# Any other URL
bunx @temps-sdk/cli --target-context my-instance sandbox create --workspace \\
  --git-url https://github.com/org/repo.git --branch develop

# Come back days later — this wakes the workspace automatically
bunx @temps-sdk/cli --target-context my-instance sandbox exec sbx_abc123 -- git status

# The image ships claude, codex, opencode, gh and glab. Supply your own
# key at create time until credential injection lands (ADR-036). Pass it by
# reference so the secret stays out of your shell history.
bunx @temps-sdk/cli --target-context my-instance sandbox create --workspace -e ANTHROPIC_API_KEY="$ANTHROPIC_API_KEY"

bunx @temps-sdk/cli --target-context my-instance sandbox list --workspace`

const REST_EXAMPLE = `# Create
curl -X POST https://your-temps-instance.com/v1/sandbox \\
  -H "Authorization: Bearer $TEMPS_API_TOKEN" \\
  -H "Content-Type: application/json" \\
  -d '{
    "name": "my-sandbox",
    "timeout_secs": 7200,
    "env": { "NODE_ENV": "development" },
    "source": {
      "type": "git",
      "repo_url": "https://github.com/org/repo.git",
      "branch": "main"
    }
  }'

# Response
# {
#   "id": "sbx_abc123",
#   "status": "running",
#   "preview_url_template": "https://sbx-abc123-{port}.preview.example.com",
#   ...
# }`

const SDK_EXAMPLE = `import { Sandbox } from '@temps-sdk/sandbox'

// Reads TEMPS_API_URL / TEMPS_API_TOKEN from the environment.
// Pass them explicitly via apiUrl/apiToken if you prefer.
const sandbox = await Sandbox.create({
  name: 'my-sandbox',
  timeoutSecs: 7200,
  source: {
    type: 'git',
    url: 'https://github.com/org/repo.git',
    revision: 'main',
  },
})

// Run a command and read stdout
const { stdout } = await sandbox.exec(['node', '--version'])
console.log(stdout)

// Build a public URL for a port exposed inside the sandbox
const previewUrl = sandbox.domain(3000)

// Clean up (use .destroy() to also delete the row)
await sandbox.stop()`

type Variant = 'full' | 'compact'

interface CreateSandboxDocsProps {
  /// `full` renders the big onboarding card used in the empty state.
  /// `compact` renders a collapsible banner above the list so the docs
  /// are still reachable once sandboxes exist, without crowding the page.
  variant?: Variant
}

export function CreateSandboxDocs({
  variant = 'full',
}: CreateSandboxDocsProps) {
  const [open, setOpen] = useState(variant === 'full')

  const body = (
    <Tabs defaultValue="cli" className="space-y-3">
      <TabsList>
        <TabsTrigger value="cli">CLI</TabsTrigger>
        <TabsTrigger value="workspace">Workspace</TabsTrigger>
        <TabsTrigger value="rest">REST</TabsTrigger>
        <TabsTrigger value="sdk">SDK</TabsTrigger>
      </TabsList>
      <TabsContent value="cli" className="space-y-2">
        <p className="text-xs text-muted-foreground">
          Run via{' '}
          <code className="bg-muted px-1 rounded">bunx @temps-sdk/cli</code> —
          no install step needed. Log in once to store a named context, then
          pass <code className="bg-muted px-1 rounded">--target-context</code>{' '}
          so every command reaches the intended server.
        </p>
        <CodeBlock code={SANDBOX_CLI_EXAMPLE} language="bash" />
      </TabsContent>
      <TabsContent value="workspace" className="space-y-2">
        <p className="text-xs text-muted-foreground">
          A <strong>workspace</strong> is a sandbox you keep. It still suspends
          after its idle timeout so it doesn&apos;t hold memory on the host, but
          the next command wakes it automatically and your files are exactly as
          you left them. Nothing deletes it until you run{' '}
          <code className="bg-muted px-1 rounded">sandbox rm</code>.
        </p>
        <CodeBlock code={SANDBOX_WORKSPACE_EXAMPLE} language="bash" />
      </TabsContent>
      <TabsContent value="rest" className="space-y-2">
        <p className="text-xs text-muted-foreground">
          Authenticate with a personal access token (create one under{' '}
          <a href="/keys" className="underline">
            API Keys
          </a>
          ). Full schema is in the OpenAPI spec at{' '}
          <code className="bg-muted px-1 rounded">/api/openapi.json</code>.
        </p>
        <CodeBlock code={REST_EXAMPLE} language="bash" />
      </TabsContent>
      <TabsContent value="sdk" className="space-y-2">
        <p className="text-xs text-muted-foreground">
          The Node SDK wraps the REST API and exposes ergonomic helpers for{' '}
          <code className="bg-muted px-1 rounded">exec</code>,{' '}
          <code className="bg-muted px-1 rounded">domain(port)</code>, file I/O,
          and detached jobs. Install with{' '}
          <code className="bg-muted px-1 rounded">
            bun add @temps-sdk/sandbox
          </code>
          . Shape is drop-in compatible with{' '}
          <code className="bg-muted px-1 rounded">@vercel/sandbox</code>.
        </p>
        <CodeBlock code={SDK_EXAMPLE} language="typescript" />
      </TabsContent>
    </Tabs>
  )

  if (variant === 'full') {
    return (
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2 text-base">
            <Terminal className="h-4 w-4" />
            Create your first sandbox
          </CardTitle>
          <CardDescription>
            Sandboxes are created via the CLI, REST API, or SDK — there is no
            create-in-UI flow yet. Pick whichever fits your workflow; all three
            hit the same endpoint.
          </CardDescription>
        </CardHeader>
        <CardContent>{body}</CardContent>
      </Card>
    )
  }

  return (
    <Card>
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        className="w-full flex items-center justify-between p-4 text-left hover:bg-muted/40 transition-colors rounded-lg"
      >
        <span className="flex items-center gap-2 text-sm font-medium">
          <Terminal className="h-4 w-4" />
          How to create a sandbox
          <span className="text-xs font-normal text-muted-foreground">
            CLI · REST · SDK
          </span>
        </span>
        {open ? (
          <ChevronDown className="h-4 w-4 text-muted-foreground" />
        ) : (
          <ChevronRight className="h-4 w-4 text-muted-foreground" />
        )}
      </button>
      {open && <CardContent className="border-t pt-4">{body}</CardContent>}
    </Card>
  )
}
