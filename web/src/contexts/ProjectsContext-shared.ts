// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ProjectResponse } from '@/api/client'
import { createContext, useContext } from 'react'

export interface ProjectsContextType {
  projects: ProjectResponse[]
  isLoading: boolean
}

export const ProjectsContext = createContext<ProjectsContextType>({
  projects: [],
  isLoading: false,
})

export function useProjects() {
  return useContext(ProjectsContext)
}
