import type { Command } from 'commander'
import { credentials } from '../config/store.js'
import { getFeatureMaturity } from '../api/sdk.gen.js'
import type { FeatureMaturity } from '../api/types.gen.js'
import { setupClient } from './api-client.js'

export type { FeatureMaturity } from '../api/types.gen.js'

const COMMAND_FEATURE_KEYS: Readonly<Record<string, string>> = {
  analytics: 'web-analytics',
  funnels: 'funnels',
  errors: 'error-tracking',
  metrics: 'otel-traces-metrics',
  traces: 'otel-traces-metrics',
  notifications: 'notifications-webhooks',
  webhooks: 'notifications-webhooks',
  teams: 'teams',
  data: 'database-browser',
  imports: 'competitor-importers',
  'ip-access': 'ip-access-geo',
  incidents: 'status-pages',
  sandbox: 'sandboxes-preview-environments',
  workflow: 'ai-agents-workflows',
  revenue: 'revenue-tracking',
  'session-replay': 'session-replay',
  scans: 'vulnerability-scanning',
  ai: 'ai-chat',
}

const announcedGroups = new Set<string>()

export function topLevelCommandName(
  actionCommand: Command,
  rootCommand: Command
): string {
  let command = actionCommand
  while (command.parent && command.parent !== rootCommand) {
    command = command.parent
  }
  return command.name()
}

export function maturityNotice(
  commandGroup: string,
  registry: readonly FeatureMaturity[]
): string | undefined {
  const featureKey = COMMAND_FEATURE_KEYS[commandGroup]
  if (!featureKey) return undefined
  const feature = registry.find((entry) => entry.key === featureKey)
  if (!feature || feature.maturity === 'stable') return undefined

  const label = feature.maturity === 'beta' ? 'Beta' : 'Experimental'
  const promise =
    feature.maturity === 'beta'
      ? 'Its data model or API may change between releases.'
      : "It may change substantially or be removed; don't build critical workflows on it yet."

  return `[temps] ${label}: ${commandGroup}. ${promise}\n${feature.reason}\n${feature.docs_path}\n`
}

async function loadFeatureMaturity(): Promise<FeatureMaturity[] | undefined> {
  const apiKey = await credentials.getApiKey()
  if (!apiKey) return undefined

  try {
    await setupClient()
    const { data, error } = await getFeatureMaturity({
      signal: AbortSignal.timeout(1_500),
    })
    if (error || !data) return undefined
    return data
  } catch {
    // Older servers do not expose maturity metadata. Never make the target
    // command fail merely because its informational notice is unavailable.
    return undefined
  }
}

export async function announceCommandMaturity(
  actionCommand: Command,
  rootCommand: Command
): Promise<void> {
  const commandGroup = topLevelCommandName(actionCommand, rootCommand)
  if (announcedGroups.has(commandGroup)) return

  const registry = await loadFeatureMaturity()
  if (!registry) return

  const notice = maturityNotice(commandGroup, registry)
  if (!notice) return

  announcedGroups.add(commandGroup)
  process.stderr.write(notice)
}

export function __resetMaturityNotices(): void {
  announcedGroups.clear()
}
