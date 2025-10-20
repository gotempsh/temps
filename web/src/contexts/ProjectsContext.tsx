import { Project } from '@/api/client'
import { getProjectsOptions } from '@/api/client/@tanstack/react-query.gen'
import { useQuery } from '@tanstack/react-query'
import { createContext, useContext } from 'react'

interface ProjectsContextType {
  projects: Project[]
  isLoading: boolean
}

const ProjectsContext = createContext<ProjectsContextType>({
  projects: [],
  isLoading: false,
})

export function ProjectsProvider({ children }: { children: React.ReactNode }) {
  const { data, isLoading } = useQuery({
    ...getProjectsOptions({
      query: {
        page: 1,
        per_page: 4,
      },
    }),
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
