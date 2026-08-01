import {
  getEnvironmentVariableValue,
  getResolvedEnvironmentVariableValue,
  getResolvedEnvironmentVariables,
  type ResolvedEnvVarResponse,
} from '@/api/client'

export type { EnvVarIntegrationInfo, ResolvedEnvVarSource } from '@/api/client'
export type ResolvedEnvVar = ResolvedEnvVarResponse

export async function getResolvedEnvVarValue(
  projectId: number,
  key: string,
  environmentId?: number,
  serviceId?: number
): Promise<string> {
  const response = await getResolvedEnvironmentVariableValue({
    path: { project_id: projectId, key },
    query: {
      environment_id: environmentId,
      service_id: serviceId,
    },
    credentials: 'include',
    cache: 'no-store',
    throwOnError: true,
  })
  return response.data.value
}

export async function getEnvVarValue(
  projectId: number,
  key: string,
  varId: number,
  environmentId?: number
): Promise<string> {
  const response = await getEnvironmentVariableValue({
    path: { project_id: projectId, key },
    query: {
      environment_id: environmentId,
      var_id: varId,
    },
    credentials: 'include',
    cache: 'no-store',
    throwOnError: true,
  })
  return response.data.value
}

export async function getResolvedEnvVars(
  projectId: number,
  environmentId?: number
): Promise<ResolvedEnvVar[]> {
  const response = await getResolvedEnvironmentVariables({
    path: { project_id: projectId },
    query: { environment_id: environmentId },
    credentials: 'include',
    cache: 'no-store',
    throwOnError: true,
  })
  return response.data
}

/**
 * Build a Map keyed by env-var key for O(1) lookup when rendering rows.
 */
export function indexResolvedByKey(
  resolved: ResolvedEnvVar[] | undefined
): Map<string, ResolvedEnvVar> {
  const map = new Map<string, ResolvedEnvVar>()
  if (!resolved) return map
  for (const entry of resolved) {
    map.set(entry.key, entry)
  }
  return map
}
