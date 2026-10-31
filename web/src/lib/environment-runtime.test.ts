// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import type { EnvironmentResponse, ProjectResponse } from '@/api/client'
import { environmentRuntimeFacts } from './environment-runtime'

const project = {
  main_branch: 'main',
  deployment_config: { automaticDeploy: false },
} as unknown as ProjectResponse

test('an environment with defaults reads as one plain sentence', () => {
  const environment = {
    branch: null,
    deployment_config: null,
  } as unknown as EnvironmentResponse
  expect(environmentRuntimeFacts(project, environment)).toEqual({
    branch: 'main',
    trigger: 'manually',
    details: [
      '1 replica',
      'no CPU limit',
      'no memory limit',
      'builds on the control plane',
    ],
  })
})

test('its own branch, sizes and build location win over the project', () => {
  const environment = {
    branch: 'release',
    deployment_config: {
      automaticDeploy: true,
      replicas: 3,
      cpuLimit: 2,
      memoryLimit: 2048,
      buildLocation: 'node',
    },
  } as unknown as EnvironmentResponse
  expect(environmentRuntimeFacts(project, environment)).toEqual({
    branch: 'release',
    trigger: 'on every push',
    details: [
      '3 replicas',
      '2 cores',
      '2 GB memory',
      'builds on a worker node',
    ],
  })
})
