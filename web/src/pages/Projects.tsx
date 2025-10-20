import { useEffect, useState } from 'react'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { ProjectCard } from '@/components/dashboard/ProjectCard'
import { ProjectCardSkeleton } from '@/components/skeletons/ProjectCardSkeleton'
import { Button } from '@/components/ui/button'
import {
  getProjectsOptions,
  listGitProvidersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { useQuery } from '@tanstack/react-query'
import { Plus, FolderPlus, GitBranch, Upload } from 'lucide-react'
import { Link } from 'react-router-dom'

const ITEMS_PER_PAGE = 8

export function Projects() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const [page, setPage] = useState(1)

  const { data: projectsData, isLoading } = useQuery({
    ...getProjectsOptions({
      query: {
        page,
        per_page: ITEMS_PER_PAGE,
      },
    }),
  })

  const { data: gitProviders, isLoading: gitProvidersLoading } = useQuery({
    ...listGitProvidersOptions({}),
    retry: false,
  })

  useEffect(() => {
    setBreadcrumbs([{ label: 'Projects' }])
  }, [setBreadcrumbs])

  usePageTitle('Projects')

  return (
    <div className="sm:p-8 space-y-6">
      {/* Header */}
      <div className="flex justify-between items-center">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">Projects</h1>
          <p className="text-sm text-muted-foreground">
            Manage your projects and their settings
          </p>
        </div>
        <div className="flex gap-2">
          <Button asChild variant="outline">
            <Link
              to="/projects/import-wizard"
              className="flex items-center gap-2"
            >
              <Upload className="h-4 w-4" />
              Import Project
            </Link>
          </Button>
          <Button asChild>
            <Link to="/projects/new" className="flex items-center gap-2">
              <Plus className="h-4 w-4" />
              New Project
            </Link>
          </Button>
        </div>
      </div>

      {/* Projects Grid */}
      <div className="grid gap-6 md:grid-cols-2">
        {isLoading || gitProvidersLoading ? (
          <>
            {Array.from({ length: ITEMS_PER_PAGE }).map((_, i) => (
              <ProjectCardSkeleton key={i} />
            ))}
          </>
        ) : projectsData?.projects.length === 0 ? (
          // Check if there are no git providers configured
          !gitProviders || gitProviders.length === 0 ? (
            <div className="col-span-2 flex flex-col items-center justify-center rounded-lg border border-dashed p-8 text-center animate-in fade-in-50">
              <div className="flex h-20 w-20 items-center justify-center rounded-full bg-muted">
                <GitBranch className="h-10 w-10 text-muted-foreground" />
              </div>
              <h2 className="mt-6 text-xl font-semibold">
                No Git providers configured
              </h2>
              <p className="mt-2 text-center text-sm text-muted-foreground max-w-md">
                Before creating projects, you need to set up a Git provider like
                GitHub or GitLab to connect your repositories.
              </p>
              <div className="flex gap-3 mt-6">
                <Button asChild>
                  <Link
                    to="/git-sources/add"
                    className="flex items-center gap-2"
                  >
                    <GitBranch className="h-4 w-4" />
                    Add Git Provider
                  </Link>
                </Button>
                <Button asChild variant="outline">
                  <Link to="/git-sources" className="flex items-center gap-2">
                    View Providers
                  </Link>
                </Button>
              </div>
            </div>
          ) : (
            <div className="col-span-2 flex flex-col items-center justify-center rounded-lg border border-dashed p-8 text-center animate-in fade-in-50">
              <div className="flex h-20 w-20 items-center justify-center rounded-full bg-muted">
                <FolderPlus className="h-10 w-10 text-muted-foreground" />
              </div>
              <h2 className="mt-6 text-xl font-semibold">
                No projects created
              </h2>
              <p className="mt-2 text-center text-sm text-muted-foreground">
                Get started by creating or importing your first project
              </p>
              <div className="flex gap-3 mt-6">
                <Button asChild>
                  <Link to="/projects/new" className="flex items-center gap-2">
                    <Plus className="h-4 w-4" />
                    New Project
                  </Link>
                </Button>
                <Button asChild variant="outline">
                  <Link
                    to="/projects/import-wizard"
                    className="flex items-center gap-2"
                  >
                    <Upload className="h-4 w-4" />
                    Import Project
                  </Link>
                </Button>
              </div>
            </div>
          )
        ) : (
          <>
            {projectsData?.projects.map((project) => (
              <ProjectCard key={project.id} project={project} />
            ))}
          </>
        )}
      </div>

      {/* Pagination - Only show if there are projects */}
      {projectsData && projectsData.projects.length > 0 && (
        <div className="flex items-center justify-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => setPage((p) => Math.max(1, p - 1))}
            disabled={page === 1}
          >
            Previous
          </Button>
          <span className="text-sm text-muted-foreground">
            Page {page} of {Math.ceil(projectsData.total / ITEMS_PER_PAGE)}
          </span>
          <Button
            variant="outline"
            size="sm"
            onClick={() => setPage((p) => p + 1)}
            disabled={page >= Math.ceil(projectsData.total / ITEMS_PER_PAGE)}
          >
            Next
          </Button>
        </div>
      )}
    </div>
  )
}
