export const AI_HARNESS_KEY_NAME = 'Temps AI harness'
export const TEMPS_CLI_VERSION = '0.1.34'

export type AiHarnessStatus = 'missing' | 'waiting' | 'connected'

export interface AiHarnessKey {
  id: number
  name: string
  role_type: string
  is_active: boolean
  last_used_at?: string | null
}

export interface AiHarnessCommands {
  installSkill: string
  connectCli: string
  verifyIdentity: string
  verifyProjects: string
}

export interface AiHarnessKeyRequest {
  name: string
  role_type: 'admin'
}

export function buildAiHarnessKeyRequest(): AiHarnessKeyRequest {
  return {
    name: AI_HARNESS_KEY_NAME,
    role_type: 'admin',
  }
}

export function findAiHarnessKey(
  keys: readonly AiHarnessKey[] | undefined
): AiHarnessKey | undefined {
  return keys?.find(
    (key) =>
      key.name === AI_HARNESS_KEY_NAME &&
      key.role_type.toLowerCase() === 'admin' &&
      key.is_active
  )
}

export function getAiHarnessStatus(
  keys: readonly AiHarnessKey[] | undefined
): AiHarnessStatus {
  const key = findAiHarnessKey(keys)
  if (!key) return 'missing'
  return key.last_used_at ? 'connected' : 'waiting'
}

function normalizeOrigin(origin: string): string {
  return origin.replace(/\/+$/, '')
}

function contextName(origin: string): string {
  try {
    return new URL(origin).hostname.replace(/[^a-z0-9-]+/gi, '-') || 'temps'
  } catch {
    return 'temps'
  }
}

/**
 * Secret-safe commands for connecting an external AI harness to Temps.
 * The API key is read without terminal echo and referenced through an
 * environment variable so the real value never appears in copied commands.
 */
export function buildAiHarnessCommands(origin: string): AiHarnessCommands {
  const instanceOrigin = normalizeOrigin(origin)
  const context = contextName(instanceOrigin)
  const cli = `bunx @temps-sdk/cli@${TEMPS_CLI_VERSION}`

  return {
    installSkill: 'bunx skills add gotempsh/temps --skill temps',
    connectCli: [
      'read -rsp "Temps API key: " TEMPS_API_KEY && echo',
      `${cli} login ${instanceOrigin} --api-key "$TEMPS_API_KEY" --context ${context}`,
      'unset TEMPS_API_KEY',
    ].join('\n'),
    verifyIdentity: `${cli} --target-context ${context} whoami`,
    verifyProjects: `${cli} --target-context ${context} projects list --json`,
  }
}
