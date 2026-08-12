import { Link } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import { ArrowRight, Bot, Gauge, Workflow } from 'lucide-react'

import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { EmptyState } from '@/components/ui/empty-state'
import { getProjectsOptions } from '@/api/client/@tanstack/react-query.gen'
import { usePageTitle } from '@/hooks/usePageTitle'

// AI Workflows (autofixer/agent runs) are configured per project, unlike
// the other AI tabs (Providers/Usage/Chats/Skills/MCP Servers) which are
// instance-wide. This page is the overview that ties them together: it
// lists projects with a direct link into each one's Workflows tab, plus a
// link to the infra-health dashboard at /agent-sandbox.
export function AiWorkflowsOverview() {
  usePageTitle('AI Workflows')

  const { data, isPending } = useQuery({
    ...getProjectsOptions({ query: { page: 1, per_page: 50 } }),
  })
  const projects = data?.projects ?? []

  return (
    <div className="container mx-auto px-4 sm:px-6 py-4 sm:py-6 space-y-4 sm:space-y-6">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h1 className="text-2xl font-semibold tracking-tight">
            AI Workflows
          </h1>
          <p className="text-sm text-muted-foreground">
            Autofixer and agent runs, configured per project.
          </p>
        </div>
        <Button variant="outline" asChild>
          <Link to="/agent-sandbox">
            <Gauge className="mr-1.5 size-4" />
            Infra status
          </Link>
        </Button>
      </div>

      {isPending ? (
        <div className="space-y-2">
          {Array.from({ length: 4 }).map((_, i) => (
            <Skeleton key={i} className="h-16 w-full" />
          ))}
        </div>
      ) : projects.length === 0 ? (
        <EmptyState
          icon={Workflow}
          title="No projects yet"
          description="Workflows run inside a project. Create a project first, then open its Workflows tab to configure an agent."
        />
      ) : (
        <Card>
          <CardHeader>
            <CardTitle>Projects</CardTitle>
            <CardDescription>
              Open a project&apos;s Workflows tab to configure or run its
              agents.
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-1">
            {projects.map((project) => (
              <Link
                key={project.id}
                to={`/projects/${project.slug}/agents`}
                className="flex items-center justify-between rounded-md px-3 py-2.5 transition-colors hover:bg-accent"
              >
                <span className="flex items-center gap-2 text-sm font-medium">
                  <Bot className="size-4 text-muted-foreground" />
                  {project.name}
                </span>
                <ArrowRight className="size-4 text-muted-foreground" />
              </Link>
            ))}
          </CardContent>
        </Card>
      )}
    </div>
  )
}
