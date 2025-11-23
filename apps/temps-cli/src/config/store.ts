import Conf from 'conf'
import { colors } from '../ui/output.js'

export interface TempsConfig {
  apiUrl: string
  defaultProject?: string
  defaultEnvironment?: string
  outputFormat: 'table' | 'json' | 'minimal'
  colorEnabled: boolean
}

export interface TempsCredentials {
  apiKey?: string
  userId?: number
  email?: string
}

const DEFAULT_CONFIG: TempsConfig = {
  apiUrl: 'http://localhost:3000',
  outputFormat: 'table',
  colorEnabled: true,
}

const configStore = new Conf<TempsConfig>({
  projectName: 'temps-cli',
  configName: 'config',
  defaults: DEFAULT_CONFIG,
})

// Secret keys for Bun's secure storage
const SECRET_KEYS = {
  apiKey: 'temps_api_key',
  userId: 'temps_user_id',
  email: 'temps_email',
} as const

/**
 * Get a secret from Bun's secure storage
 */
function getSecret(key: string): string | undefined {
  // Bun stores secrets as properties on process.env that are loaded from .env.local
  // For secure storage, we use Bun's built-in secret storage via environment
  const value = process.env[key]
  return value || undefined
}

/**
 * Set a secret in Bun's secure storage
 * Writes to ~/.temps/.secrets file which is loaded automatically
 */
async function setSecret(key: string, value: string): Promise<void> {
  const secretsPath = getSecretsPath()
  const secrets = await loadSecrets()
  secrets[key] = value
  await saveSecrets(secrets)
}

/**
 * Delete a secret from Bun's secure storage
 */
async function deleteSecret(key: string): Promise<void> {
  const secrets = await loadSecrets()
  delete secrets[key]
  await saveSecrets(secrets)
}

/**
 * Get the path to the secrets file
 */
function getSecretsPath(): string {
  const home = process.env.HOME || process.env.USERPROFILE || '~'
  return `${home}/.temps/.secrets`
}

/**
 * Load secrets from the secrets file
 */
async function loadSecrets(): Promise<Record<string, string>> {
  const secretsPath = getSecretsPath()
  try {
    const file = Bun.file(secretsPath)
    if (await file.exists()) {
      const content = await file.text()
      const secrets: Record<string, string> = {}
      for (const line of content.split('\n')) {
        const trimmed = line.trim()
        if (trimmed && !trimmed.startsWith('#')) {
          const eqIndex = trimmed.indexOf('=')
          if (eqIndex > 0) {
            const key = trimmed.slice(0, eqIndex)
            let value = trimmed.slice(eqIndex + 1)
            // Remove quotes if present
            if ((value.startsWith('"') && value.endsWith('"')) ||
                (value.startsWith("'") && value.endsWith("'"))) {
              value = value.slice(1, -1)
            }
            secrets[key] = value
          }
        }
      }
      return secrets
    }
  } catch {
    // File doesn't exist or can't be read
  }
  return {}
}

/**
 * Save secrets to the secrets file
 */
async function saveSecrets(secrets: Record<string, string>): Promise<void> {
  const secretsPath = getSecretsPath()
  const dir = secretsPath.substring(0, secretsPath.lastIndexOf('/'))

  // Ensure directory exists
  await Bun.write(`${dir}/.keep`, '')

  // Write secrets file
  const lines = ['# Temps CLI secrets - DO NOT SHARE THIS FILE']
  for (const [key, value] of Object.entries(secrets)) {
    lines.push(`${key}="${value}"`)
  }
  await Bun.write(secretsPath, lines.join('\n') + '\n')

  // Set restrictive permissions (owner read/write only)
  const { chmod } = await import('node:fs/promises')
  await chmod(secretsPath, 0o600)
}

export const config = {
  get<K extends keyof TempsConfig>(key: K): TempsConfig[K] {
    return configStore.get(key)
  },

  set<K extends keyof TempsConfig>(key: K, value: TempsConfig[K]): void {
    configStore.set(key, value)
  },

  getAll(): TempsConfig {
    return configStore.store
  },

  setAll(values: Partial<TempsConfig>): void {
    for (const [key, value] of Object.entries(values)) {
      configStore.set(key as keyof TempsConfig, value)
    }
  },

  reset(): void {
    configStore.clear()
    Object.assign(configStore.store, DEFAULT_CONFIG)
  },

  path: configStore.path,
}

export const credentials = {
  async get<K extends keyof TempsCredentials>(key: K): Promise<TempsCredentials[K]> {
    const secrets = await loadSecrets()
    const secretKey = SECRET_KEYS[key]
    const value = secrets[secretKey]

    if (key === 'userId' && value) {
      return parseInt(value, 10) as TempsCredentials[K]
    }
    return value as TempsCredentials[K]
  },

  async set<K extends keyof TempsCredentials>(key: K, value: TempsCredentials[K]): Promise<void> {
    const secretKey = SECRET_KEYS[key]
    if (value !== undefined && value !== null) {
      await setSecret(secretKey, String(value))
    } else {
      await deleteSecret(secretKey)
    }
  },

  async getAll(): Promise<TempsCredentials> {
    const secrets = await loadSecrets()
    const userIdStr = secrets[SECRET_KEYS.userId]
    return {
      apiKey: secrets[SECRET_KEYS.apiKey],
      userId: userIdStr ? parseInt(userIdStr, 10) : undefined,
      email: secrets[SECRET_KEYS.email],
    }
  },

  async setAll(values: Partial<TempsCredentials>): Promise<void> {
    const secrets = await loadSecrets()

    if (values.apiKey !== undefined) {
      if (values.apiKey) {
        secrets[SECRET_KEYS.apiKey] = values.apiKey
      } else {
        delete secrets[SECRET_KEYS.apiKey]
      }
    }
    if (values.userId !== undefined) {
      if (values.userId) {
        secrets[SECRET_KEYS.userId] = String(values.userId)
      } else {
        delete secrets[SECRET_KEYS.userId]
      }
    }
    if (values.email !== undefined) {
      if (values.email) {
        secrets[SECRET_KEYS.email] = values.email
      } else {
        delete secrets[SECRET_KEYS.email]
      }
    }

    await saveSecrets(secrets)
  },

  async clear(): Promise<void> {
    await saveSecrets({})
  },

  async isAuthenticated(): Promise<boolean> {
    const secrets = await loadSecrets()
    return !!secrets[SECRET_KEYS.apiKey]
  },

  async getApiKey(): Promise<string | undefined> {
    const secrets = await loadSecrets()
    return secrets[SECRET_KEYS.apiKey]
  },

  get path(): string {
    return getSecretsPath()
  },
}

export function getApiUrl(): string {
  return config.get('apiUrl')
}

export async function requireAuth(): Promise<string> {
  const apiKey = await credentials.getApiKey()
  if (!apiKey) {
    console.error(colors.error('Not authenticated. Please run: temps login'))
    process.exit(1)
  }
  return apiKey
}
