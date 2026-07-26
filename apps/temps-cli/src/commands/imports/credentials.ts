/**
 * Credential collection for import sources.
 *
 * Coolify/Dokploy/CapRover/Portainer are self-hosted platforms: importing
 * from them needs a base URL plus a token (or admin password, for CapRover/
 * Portainer) scoped to that instance. Kubernetes needs a full kubeconfig.
 * Docker (the local daemon) and Kamal (deploys from the developer's own
 * checkout) need nothing at all.
 */

import { readFileSync, existsSync } from 'node:fs'
import type { ImportCredentials, ImportSource } from '../../api/types.gen.js'
import { promptText, promptPassword, promptConfirm } from '../../ui/prompts.js'
import { colors, icons, info, warning } from '../../ui/output.js'

export interface CredentialFlags {
  token?: string
  baseUrl?: string
  username?: string
  kubeconfig?: string
}

/**
 * Sources that need a base URL + bearer token (or, for CapRover/Portainer,
 * an admin password sent as the token) to reach a self-hosted instance.
 */
const TOKEN_AND_URL_SOURCES = new Set<ImportSource>(['coolify', 'dokploy', 'caprover', 'portainer'])

function tokenLabel(source: ImportSource): string {
  switch (source) {
    case 'caprover':
    case 'portainer':
      return 'admin password'
    default:
      return 'API token'
  }
}

function usernameNeeded(source: ImportSource): boolean {
  return source === 'portainer'
}

/**
 * Resolve credentials for a source, prompting interactively for whatever
 * `flags` didn't already supply. Never echoes a token/password back to the
 * terminal — masked input only, and nothing is logged.
 */
export async function resolveCredentials(
  source: ImportSource,
  flags: CredentialFlags,
): Promise<ImportCredentials> {
  if (source === 'kubernetes') {
    return resolveKubernetesCredentials(flags)
  }

  if (!TOKEN_AND_URL_SOURCES.has(source)) {
    // docker (local daemon), kamal (deploys from a local checkout, nothing
    // to authenticate to), and anything else with no credential concept.
    return {}
  }

  const baseUrl =
    flags.baseUrl ??
    (await promptText({
      message: `${source} instance URL (e.g. https://${source}.example.com)`,
      required: true,
      validate: (value) => {
        try {
          new URL(value)
          return true
        } catch {
          return 'Enter a full URL including http(s)://'
        }
      },
    }))

  const extra: Record<string, string> = {}
  if (usernameNeeded(source)) {
    extra.username =
      flags.username ??
      (await promptText({
        message: `${source} username`,
        default: 'admin',
        required: true,
      }))
  }

  const token =
    flags.token ??
    (await promptPassword({
      message: `${source} ${tokenLabel(source)}`,
      validate: (value) => (value.length > 0 ? true : 'Required'),
    }))

  return { token, base_url: baseUrl, extra }
}

async function resolveKubernetesCredentials(flags: CredentialFlags): Promise<ImportCredentials> {
  let kubeconfigContent: string

  if (flags.kubeconfig) {
    kubeconfigContent = readKubeconfigFile(flags.kubeconfig)
  } else {
    info('Kubernetes needs a kubeconfig with access to the source cluster.')
    const path = await promptText({
      message: 'Path to kubeconfig file',
      default: process.env.KUBECONFIG ?? '~/.kube/config',
      required: true,
    })
    kubeconfigContent = readKubeconfigFile(path)
  }

  if (kubeconfigContent.includes('insecure-skip-tls-verify: true') || kubeconfigContent.includes('insecure-skip-tls-verify:true')) {
    warning('This kubeconfig disables TLS certificate verification for the cluster connection.')
    const proceed = await promptConfirm({
      message: 'Continue anyway?',
      default: false,
    })
    if (!proceed) {
      throw new Error('Cancelled — kubeconfig has insecure-skip-tls-verify set')
    }
  }

  return { extra: { kubeconfig: kubeconfigContent } }
}

function readKubeconfigFile(path: string): string {
  const expanded = path.startsWith('~')
    ? path.replace('~', process.env.HOME ?? '~')
    : path
  if (!existsSync(expanded)) {
    throw new Error(`Kubeconfig not found at ${expanded}`)
  }
  return readFileSync(expanded, 'utf-8')
}

/**
 * A short, non-secret description of what was supplied — safe to print for
 * "here's what I'm about to use" confirmation, since it never includes the
 * token/password itself.
 */
export function describeCredentials(source: ImportSource, credentials: ImportCredentials): string | null {
  if (credentials.base_url) {
    return `${colors.muted(icons.lock)} ${credentials.base_url}`
  }
  if (credentials.extra?.kubeconfig) {
    return `${colors.muted(icons.lock)} kubeconfig (${credentials.extra.kubeconfig.length} bytes)`
  }
  return null
}
