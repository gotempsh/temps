// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createContext, useContext } from 'react'

export interface OpenDeployOptions {
  /** The environment the dialog starts on; its branch comes with it. */
  environmentId?: number
  /**
   * Every environment waiting for this deploy, for an upload: it leaves the
   * page that asked, so the upload page has to carry on with the rest.
   */
  pendingEnvironmentIds?: number[]
  /**
   * Called once the deploy has started, instead of leaving for the deployments
   * list, for a page that has more to ask about.
   */
  onStarted?: (environmentId: number) => void
}

export interface ProjectDeployContextValue {
  /**
   * Opens the project's deploy dialog (or its upload page, for a project
   * deployed by uploading), the same one the environment bar's Deploy opens,
   * so a page asking for a deploy shows what will be deployed first.
   */
  openDeploy: (options?: OpenDeployOptions) => void
}

export const ProjectDeployContext =
  createContext<ProjectDeployContextValue | null>(null)

/** The project's deploy dialog, or `null` outside a project page. */
export function useProjectDeploy(): ProjectDeployContextValue | null {
  return useContext(ProjectDeployContext)
}
