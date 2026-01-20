import { useEffect, useState } from 'react'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useAuth } from '@/contexts/AuthContext'
import { useKeyboardShortcut } from '@/hooks/useKeyboardShortcut'
import { usePageTitle } from '@/hooks/usePageTitle'
import { ProjectCard } from '@/components/dashboard/ProjectCard'
import { ProjectCardSkeleton } from '@/components/skeletons/ProjectCardSkeleton'
import { Button } from '@/components/ui/button'
import { KbdBadge } from '@/components/ui/kbd-badge'
import {
  getProjectsOptions,
  listGitProvidersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { useQuery } from '@tanstack/react-query'
import { Plus, FolderPlus, GitBranch, Upload } from 'lucide-react'
import { Link, useNavigate } from 'react-router-dom'

const ITEMS_PER_PAGE = 8

export function Projects() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const { isDemoMode } = useAuth()
  const navigate = useNavigate()
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

  // Keyboard shortcut: N to create new project
  useKeyboardShortcut({ key: 'n', path: '/projects/new' })

  // Keyboard shortcuts: Ctrl+1 through Ctrl+9 to navigate to projects
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      // Check if user is typing in an input field
      const target = e.target as HTMLElement
      const isTyping =
        target.tagName === 'INPUT' ||
        target.tagName === 'TEXTAREA' ||
        target.isContentEditable

      // Only trigger if Ctrl (or Cmd on Mac) is pressed with a number key
      if (
        !isTyping &&
        (e.ctrlKey || e.metaKey) &&
        !e.altKey &&
        !e.shiftKey &&
        e.key >= '1' &&
        e.key <= '9'
      ) {
        const index = parseInt(e.key, 10) - 1
        const projects = projectsData?.projects || []

        if (projects[index]) {
          e.preventDefault()
          navigate(`/projects/${projects[index].slug}`)
        }
      }
    }

    document.addEventListener('keydown', handleKeyDown)
    return () => document.removeEventListener('keydown', handleKeyDown)
  }, [projectsData?.projects, navigate])

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
        {!isDemoMode && (
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
                <KbdBadge keys="N" />
              </Link>
            </Button>
          </div>
        )}
      </div>

      {/* Projects Grid */}
      <div className="grid gap-6 md:grid-cols-2 lg:grid-cols-3">
        {isLoading || gitProvidersLoading ? (
          <>
            {Array.from({ length: ITEMS_PER_PAGE }).map((_, i) => (
              <ProjectCardSkeleton key={i} />
            ))}
          </>
        ) : projectsData?.projects.length === 0 ? (
          isDemoMode ? (
            // Demo mode: simple empty state without action buttons
            <div className="col-span-full flex flex-col items-center justify-center rounded-lg border border-dashed p-8 text-center animate-in fade-in-50">
              <div className="flex h-20 w-20 items-center justify-center rounded-full bg-muted">
                <FolderPlus className="h-10 w-10 text-muted-foreground" />
              </div>
              <h2 className="mt-6 text-xl font-semibold">No projects</h2>
              <p className="mt-2 text-center text-sm text-muted-foreground">
                No projects are available in demo mode.
              </p>
            </div>
          ) : // Check if there are no git providers configured
          !gitProviders || gitProviders.length === 0 ? (
            <div className="col-span-full flex flex-col items-center justify-center rounded-lg border border-dashed p-8 text-center animate-in fade-in-50">
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
            <div className="col-span-full flex flex-col items-center justify-center rounded-lg border border-dashed p-8 text-center animate-in fade-in-50">
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
            {projectsData?.projects.map((project, index) => (
              <ProjectCard
                key={project.id}
                project={project}
                shortcutNumber={index < 9 ? index + 1 : undefined}
              />
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
