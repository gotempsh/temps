// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Tool the assistant calls to deploy the sandbox's files as a project. */
export const SANDBOX_DEPLOY_TOOL_NAME = 'temps_deploy_sandbox_source'

export function isSandboxDeployTool(name: string): boolean {
  return (
    name === SANDBOX_DEPLOY_TOOL_NAME ||
    name.endsWith(`__${SANDBOX_DEPLOY_TOOL_NAME}`)
  )
}

/** A deployment the tool started, read from its result. */
export interface SandboxDeployStarted {
  deploymentId: number
  projectId: number
  environmentId: number | null
  /** Files packed from the sandbox. */
  files: number | null
  /** Files left out (dependencies, build output, credentials). */
  leftOut: number | null
}

const positive = (value: unknown): number | null =>
  typeof value === 'number' && Number.isSafeInteger(value) && value > 0
    ? value
    : null

const count = (value: unknown): number | null =>
  typeof value === 'number' && Number.isSafeInteger(value) && value >= 0
    ? value
    : null

/**
 * `null` while the call is running (approval, packing) and when it did not
 * start a deployment: a refusal or failure is a sentence, which the generic
 * tool card shows as it is.
 */
export function parseSandboxDeployStarted(
  result: string | null | undefined
): SandboxDeployStarted | null {
  if (!result) return null
  let value: unknown
  try {
    value = JSON.parse(result)
  } catch {
    return null
  }
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    return null
  }
  const record = value as Record<string, unknown>
  if (record.status !== 'started') return null
  const deploymentId = positive(record.deployment_id)
  const projectId = positive(record.project_id)
  if (!deploymentId || !projectId) return null
  return {
    deploymentId,
    projectId,
    environmentId: positive(record.environment_id),
    files: count(record.files),
    leftOut: count(record.left_out),
  }
}

/** One line for the card: what was sent from the sandbox. */
export function sandboxDeploySummary(started: SandboxDeployStarted): string {
  if (started.files === null) return 'Deploying the files from the sandbox'
  const files = `${started.files} ${started.files === 1 ? 'file' : 'files'}`
  return started.leftOut
    ? `Deploying ${files} from the sandbox (${started.leftOut} left out: dependencies, build output, credentials)`
    : `Deploying ${files} from the sandbox`
}
