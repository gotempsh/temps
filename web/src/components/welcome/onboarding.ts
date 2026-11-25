// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * First run, in the order that reaches value fastest: the app live at an
 * address with an agent watching it. Only what that needs is asked now; the
 * rest waits on the Setup page. The state lives on the server
 * (`GET/PATCH /settings/onboarding`), so every admin and every browser sees
 * the same progress.
 */

import { consoleReach } from '@/lib/network-address'

export type StepKey = 'host' | 'reach' | 'agents' | 'app'

export const STEPS: Array<{ key: StepKey; n: string; title: string }> = [
  { key: 'host', n: '01', title: 'Check this machine' },
  { key: 'reach', n: '02', title: 'Reach your apps' },
  { key: 'agents', n: '03', title: 'Connect an agent' },
  { key: 'app', n: '04', title: 'Deploy an app' },
]

/** The subset of the server's onboarding record the flow reads. */
export interface OnboardingLike {
  completed_at?: string | null
  done?: StepKey[]
  later?: Array<{ step: StepKey; reason: string }>
}

/** The next step neither done nor put off, in order; null once all are settled. */
export function nextStep(state: OnboardingLike): StepKey | null {
  const settled = new Set<StepKey>([
    ...(state.done ?? []),
    ...(state.later ?? []).map((l) => l.step),
  ])
  return STEPS.find((step) => !settled.has(step.key))?.key ?? null
}

/** Whether the first-run flow should open on its own for this console. */
export function shouldWelcome(
  state: OnboardingLike | undefined,
  projectCount: number | undefined
): boolean {
  if (!state || projectCount === undefined) return false
  return !state.completed_at && projectCount === 0
}

/** Where one step stands, in words for the Setup list. */
export function stepStatus(
  state: OnboardingLike,
  step: StepKey
): { kind: 'done' | 'later' | 'todo'; note?: string } {
  if (state.done?.includes(step)) return { kind: 'done' }
  const later = state.later?.find((l) => l.step === step)
  if (later) return { kind: 'later', note: later.reason }
  return { kind: 'todo' }
}

/** What a step marked "later" says it is waiting for, so the Setup page can ask again. */
export const LATER_REASONS: Record<StepKey, string> = {
  host: 'Checked later',
  reach: 'Apps stay on this network until a way in is chosen',
  agents: 'No agent connected yet',
  app: 'Nothing deployed yet',
}

/**
 * The commands for connecting the agent you already use: install the skill
 * for your user (`-g`, so no repository is needed; `-y`, so an agent running
 * it is not stopped by a question), then log the CLI in.
 */
export function harnessCommands(consoleOrigin: string): string[] {
  return [
    'bunx skills add gotempsh/temps --skill temps -g -y',
    `bunx @temps-sdk/cli login ${consoleOrigin}`,
  ]
}

/**
 * One line under the prompt saying where the agent must run, or null for a
 * public console that any agent can reach.
 */
export function reachHint(consoleOrigin: string): string | null {
  switch (consoleReach(consoleOrigin)) {
    case 'loopback':
      return 'This console is on localhost, so your agent must run on this computer.'
    case 'private':
      return "This console is on a private network, so your agent must run on a machine that reaches it. Cloud agents and the Claude app's shell can't."
    case 'public':
      return null
  }
}

/** Where the agent must run to reach this console, as prompt lines. */
function whereToRun(consoleOrigin: string): string[] {
  return {
    loopback: [
      `The server runs on my computer at a localhost address. Run these commands on my computer, in a shell that can reach ${consoleOrigin}; a cloud workspace or a VM cannot.`,
      '',
    ],
    private: [
      `The server is on a private network. Run these commands on a machine that can reach ${consoleOrigin}, such as my computer; a cloud workspace or a VM usually cannot.`,
      '',
    ],
    public: [],
  }[consoleReach(consoleOrigin)]
}

/**
 * A prompt to paste into the agent you already use. It checks it can reach
 * this console first (an agent in a cloud workspace or a VM often cannot
 * reach a localhost console), installs the skill for the user, logs the CLI
 * in with your approval in the browser, checks the connection and says what
 * it can now do. It stops on a failure rather than working around it.
 */
export function connectAgentPrompt(consoleOrigin: string): string {
  const [skill, login] = harnessCommands(consoleOrigin)
  const where = whereToRun(consoleOrigin)
  return [
    `Connect yourself to my Temps server at ${consoleOrigin}, so you can deploy and run my apps on it.`,
    '',
    ...where,
    '1. Check that you can reach the server:',
    `   curl -sS -o /dev/null -w "%{http_code}\\n" ${consoleOrigin}`,
    `   Any HTTP status means it is reachable. If curl cannot connect, tell me you cannot reach ${consoleOrigin} from where you run, and stop.`,
    '2. Install the Temps skill for my user, so it works in every repository:',
    `   ${skill}`,
    '3. Log the Temps CLI in to the server:',
    `   ${login}`,
    '   It prints a link and tries to open my browser. Show me the link, ask me to approve the login there, then wait for the command to finish. Do not use --api-key.',
    '4. Check the connection:',
    '   bunx @temps-sdk/cli whoami',
    `   It should show that you are logged in to ${consoleOrigin}.`,
    '5. Read the Temps skill, then tell me in two or three sentences what you can now do on Temps.',
    '',
    'If a command fails, show me its output and stop. Do not work around it.',
  ].join('\n')
}

/**
 * What to ask an agent for the first deploy. It works whether or not the
 * agent is logged in yet: it checks first, and only when it isn't does it
 * install the skill and log in, with your approval in the browser, as the
 * connect prompt does. An agent that reaches Temps through MCP tools uses
 * those instead. Then the agent interviews you, one question at a time, about
 * what to deploy or build, says back its plan, and changes nothing on Temps
 * until you confirm. The console watches for the project it creates.
 */
export function deployAppPrompt(consoleOrigin: string): string {
  const [skill, login] = harnessCommands(consoleOrigin)
  return [
    `Help me put my first app live on my Temps server at ${consoleOrigin}.`,
    '',
    ...whereToRun(consoleOrigin),
    '1. Check that you are logged in to it:',
    '   bunx @temps-sdk/cli whoami',
    `   If it shows you logged in to ${consoleOrigin}, go to step 2. Otherwise:`,
    `   - install the Temps skill for my user: ${skill}`,
    `   - log the Temps CLI in: ${login}`,
    '     It prints a link and tries to open my browser. Show me the link, ask me to approve the login there, then wait for the command to finish. Do not use --api-key.',
    '   If you reach Temps through its MCP tools instead, use those and skip this step.',
    '2. Ask me what I want, one question at a time, and wait for each answer. Do not assume.',
    '   - Do I want to deploy an app I already have, or build something new?',
    '   - An app I have: where is it (a folder on this computer, or a Git repository)? Look at it yourself, then tell me what you found (language, framework, how it starts) and ask only what the code cannot tell you.',
    '   - Something new: what should it do, and who is it for? Do I have a language or framework in mind? Then keep it small enough to be live today.',
    '   - Does it need a database, file storage, or secrets such as API keys? Never ask me to paste a secret into this chat; tell me where to set it in Temps instead.',
    '   - Who should reach it: anyone on the internet, or only me and my devices? Does it need its own domain?',
    '3. Say back what you will do in a few lines: the project name, what you will create on Temps (the project, any database), and where the app will be. Wait for my yes before you change anything on Temps.',
    '4. Read the Temps skill, then do it: build the app if it is new, create the project, deploy it, and tell me its address once it is live.',
    '',
    'If a command fails, show me its output and stop. Do not work around it.',
  ].join('\n')
}

/** Bytes as the largest unit that keeps a number above one, for the host facts. */
export function formatBytes(bytes: number | null | undefined): string {
  if (bytes == null || !Number.isFinite(bytes) || bytes < 0) return 'unknown'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit++
  }
  return `${value >= 10 || unit === 0 ? Math.round(value) : value.toFixed(1)} ${units[unit]}`
}
