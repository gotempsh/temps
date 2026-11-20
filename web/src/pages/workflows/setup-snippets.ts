// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The code the workflows setup guide shows. Each snippet is complete for its
 * stack and uses only exports the SDKs ship (`sdks/node`, `sdks/python`,
 * `sdks/go`): the executor endpoints live under
 * `/.well-known/temps/workflows/`, and the credential arrives as
 * `TEMPS_WORKFLOWS_TOKEN` beside the `TEMPS_API_URL` every deployment has.
 */

export type SetupStack = 'node' | 'nextjs' | 'python' | 'go'

export const setupStacks: { value: SetupStack; label: string }[] = [
  { value: 'node', label: 'Node.js' },
  { value: 'nextjs', label: 'Next.js' },
  { value: 'python', label: 'Python' },
  { value: 'go', label: 'Go' },
]

export interface SetupTarget {
  projectId?: number
  environmentId?: number
}

/** The scaffold command: a runnable Node worker with a test and a Dockerfile. */
export const scaffoldCommand = `temps workflows init my-workflows
cd my-workflows && npm install && npm test`

export function defineSnippet(stack: SetupStack): {
  code: string
  language: 'typescript' | 'python' | 'go'
} {
  switch (stack) {
    case 'node':
      return {
        language: 'typescript',
        code: `// worker.ts
import { createServer } from 'node:http'
import { createWorkflowHandler, defineWorkflow, toNodeListener } from '@temps-sdk/workflows'

export const onboarding = defineWorkflow('onboarding', async (ctx) => {
  await ctx.step('send-welcome', () => sendWelcomeEmail(ctx.input.email))
  await ctx.sleep('wait-a-day', '1d')
  await ctx.step('send-tips', () => sendTipsEmail(ctx.input.email))
})

// Serves /.well-known/temps/workflows/*; reads TEMPS_API_URL and TEMPS_WORKFLOWS_TOKEN.
const handler = createWorkflowHandler({ workflows: [onboarding] })
createServer(toNodeListener(handler)).listen(Number(process.env.PORT || 3000))`,
      }
    case 'nextjs':
      return {
        language: 'typescript',
        code: `// src/app/.well-known/temps/workflows/[action]/route.ts
import { createWorkflowHandler, defineWorkflow, type FetchHandler } from '@temps-sdk/workflows'

// Not exported: a Next.js route file may only export its handlers.
const onboarding = defineWorkflow('onboarding', async (ctx) => {
  await ctx.step('send-welcome', () => sendWelcomeEmail(ctx.input.email))
  await ctx.sleep('wait-a-day', '1d')
  await ctx.step('send-tips', () => sendTipsEmail(ctx.input.email))
})

// Created on the first request: \`next build\` runs without TEMPS_API_URL.
let handler: FetchHandler | undefined
const workflows: FetchHandler = (request) =>
  (handler ??= createWorkflowHandler({ workflows: [onboarding] }))(request)

export const GET = workflows
export const POST = workflows`,
      }
    case 'python':
      return {
        language: 'python',
        code: `# main.py — serve with: uvicorn main:app --port 3000
import os
from temps_workflows import Workflow, WorkflowApp

async def onboarding(ctx):
    await ctx.step("send-welcome", lambda: send_welcome_email(ctx.input["email"]))
    await ctx.sleep("wait-a-day", 86_400)
    await ctx.step("send-tips", lambda: send_tips_email(ctx.input["email"]))

app = WorkflowApp(
    [Workflow("onboarding", onboarding)],
    os.environ["TEMPS_API_URL"],
    os.environ["TEMPS_WORKFLOWS_TOKEN"],
)`,
      }
    case 'go':
      return {
        language: 'go',
        code: `// main.go
package main

import (
	"context"
	"log"
	"net/http"
	"os"
	"time"

	workflows "github.com/gotempsh/temps/sdks/go/workflows"
)

func onboarding(ctx *workflows.Context) (any, error) {
	email := ctx.Input.(map[string]any)["email"].(string)
	if _, err := ctx.Step("send-welcome", func(context.Context) (any, error) { return nil, sendWelcomeEmail(email) }); err != nil {
		return nil, err
	}
	if err := ctx.Sleep("wait-a-day", 24*time.Hour); err != nil {
		return nil, err
	}
	return ctx.Step("send-tips", func(context.Context) (any, error) { return nil, sendTipsEmail(email) })
}

func main() {
	client := workflows.NewClient(os.Getenv("TEMPS_API_URL"), os.Getenv("TEMPS_WORKFLOWS_TOKEN"))
	handler, err := workflows.NewHandler(context.Background(), []workflows.Workflow{{Name: "onboarding", Run: onboarding}}, client)
	if err != nil {
		log.Fatal(err)
	}
	log.Fatal(http.ListenAndServe(":3000", handler))
}`,
      }
  }
}

/** Forward this environment's invocations to a server on this machine. */
export function devCommand({ projectId, environmentId }: SetupTarget): string {
  return `temps workflows dev --project-id ${projectId ?? '<project-id>'} --environment-id ${environmentId ?? '<environment-id>'} --port 3000`
}

/** Start a run from the app: one sequence per user, however often it is called. */
export const startSnippet = `import { WorkflowsClient } from '@temps-sdk/workflows'

const client = new WorkflowsClient()
await client.start('onboarding', { email: user.email }, { idempotencyKey: \`onboarding:\${user.id}\` })`
