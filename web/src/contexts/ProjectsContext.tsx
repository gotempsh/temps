// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getProjects, ProjectResponse } from '@/api/client'
import { useQuery } from '@tanstack/react-query'
import { createContext, useContext } from 'react'
import { useAuth } from '@/contexts/AuthContext'

interface ProjectsContextType {
  projects: ProjectResponse[]
  isLoading: boolean
}

const ProjectsContext = createContext<ProjectsContextType>({
  projects: [],
  isLoading: false,
})

export function ProjectsProvider({ children }: { children: React.ReactNode }) {
  const { user } = useAuth()
  const { data, isLoading } = useQuery({
    // The generated `getProjectsOptions()` key carries no identity, so a
    // session that expires and is followed by a different account signing
    // in (no full page reload in between) would re-enable that same cache
    // entry and could serve the previous account's cached projects while
    // this query's background refetch is in flight. Scoping the key to
    // `user?.id` directly (instead of overriding the generated options'
    // key, which is typed as a fixed-shape tuple) gives every account its
    // own cache slot, so switching accounts always starts from empty.
    queryKey: ['getProjects', { page: 1, per_page: 4 }, user?.id] as const,
    queryFn: async ({ signal }) => {
      const { data } = await getProjects({
        query: { page: 1, per_page: 4 },
        signal,
        throwOnError: true,
      })
      return data
    },
    // ProjectsProvider wraps the whole app, including the logged-out login
    // screen. Without this, every logged-out visitor's browser fires this
    // authenticated query, gets a 401, and (via App.tsx's QueryCache.onError)
    // invalidates the current-user query -- which flips AuthContext's
    // loading state and remounts <Login />, wiping in-progress form input.
    enabled: !!user,
  })

  return (
    <ProjectsContext.Provider
      value={{ projects: data?.projects || [], isLoading }}
    >
      {children}
    </ProjectsContext.Provider>
  )
}

export function useProjects() {
  return useContext(ProjectsContext)
}
