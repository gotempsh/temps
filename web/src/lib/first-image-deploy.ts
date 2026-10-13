// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { defaultDeployEnvironment } from './project-deploy-action'

export type FirstImageDeployResult =
  | { status: 'skipped' }
  | { status: 'started'; environmentName: string }
  | { status: 'no_environment' }

interface FirstImageDeployDeps<
  TEnv extends { id: number; name: string; slug: string; is_preview: boolean },
> {
  listEnvironments: (projectId: number) => Promise<readonly TEnv[]>
  deployImage: (args: {
    projectId: number
    environmentId: number
    imageRef: string
  }) => Promise<unknown>
}

/**
 * Deploy the image a user entered while creating a project, so the project
 * comes up straight away instead of the image being silently discarded.
 *
 * Errors from the API propagate to the caller, which decides how to surface
 * them; the project itself has already been created by then.
 */
export async function startFirstImageDeploy<
  TEnv extends { id: number; name: string; slug: string; is_preview: boolean },
>(
  projectId: number,
  imageRef: string | undefined,
  deps: FirstImageDeployDeps<TEnv>
): Promise<FirstImageDeployResult> {
  const ref = imageRef?.trim()
  if (!ref) return { status: 'skipped' }

  const environment = defaultDeployEnvironment(
    await deps.listEnvironments(projectId)
  )
  if (!environment) return { status: 'no_environment' }

  await deps.deployImage({
    projectId,
    environmentId: environment.id,
    imageRef: ref,
  })
  return { status: 'started', environmentName: environment.name }
}
