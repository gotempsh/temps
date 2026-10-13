// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  createProject,
  deployFromImage,
  getEnvironments,
  getProjects,
} from '@/api/client'
import { startFirstImageDeploy } from '@/lib/first-image-deploy'
import { sampleProjectName, startSampleDeploy } from '@/lib/first-deploy'

/**
 * How many existing project names are checked when picking the sample's name.
 * A collision past this page only costs a clear "name already exists" error.
 */
const NAME_LOOKUP_PAGE_SIZE = 100

const listEnvironments = async (projectId: number) => {
  const { data } = await getEnvironments({
    path: { project_id: projectId },
    throwOnError: true,
  })
  return data
}

const deployImage = ({
  projectId,
  environmentId,
  imageRef,
}: {
  projectId: number
  environmentId: number
  imageRef: string
}) =>
  deployFromImage({
    path: { project_id: projectId, environment_id: environmentId },
    body: { image_ref: imageRef },
    throwOnError: true,
  })

/** Create the sample project and start its first deployment. */
export async function createAndDeploySample() {
  const { data: existing } = await getProjects({
    query: { page: 1, per_page: NAME_LOOKUP_PAGE_SIZE },
    throwOnError: true,
  })
  const name = sampleProjectName(existing.projects.map((p) => p.name))

  return startSampleDeploy(name, {
    createProject: async ({ name, port }) => {
      const { data } = await createProject({
        body: {
          name,
          // A Flexible project: it deploys the sample image now and can be
          // pointed at a repository later without being recreated.
          source_type: 'manual',
          preset: 'dockerfile',
          directory: './',
          main_branch: 'main',
          project_type: 'docker',
          automatic_deploy: false,
          exposed_port: port,
        },
        throwOnError: true,
      })
      return data
    },
    listEnvironments,
    deployImage,
  })
}

/** Deploy an image to an existing project's default environment again. */
export function redeployImage(projectId: number, imageRef: string) {
  return startFirstImageDeploy(projectId, imageRef, {
    listEnvironments,
    deployImage,
  })
}
