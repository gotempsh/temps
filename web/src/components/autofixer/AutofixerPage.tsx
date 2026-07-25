import { ProjectResponse } from '@/api/client'
import { listErrorGroupsOptions } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useQuery } from '@tanstack/react-query'
import { Sparkles, Wand2 } from 'lucide-react'
import { useNavigate } from 'react-router'

interface AutofixerPageProps {
  project: ProjectResponse
}

function formatTimeAgo(dateStr: string): string {
  const diffMs = Date.now() - new Date(dateStr).getTime()
  const diffMins = Math.floor(diffMs / 60000)
  if (diffMins < 60) return `${diffMins}m ago`
  const diffHours = Math.floor(diffMins / 60)
  if (diffHours < 24) return `${diffHours}h ago`
  return `${Math.floor(diffHours / 24)}d ago`
}

export function AutofixerPage({ project }: AutofixerPageProps) {
  const navigate = useNavigate()

  // Fetch unresolved error groups to show fixable errors
  const { data: errorGroups, isLoading } = useQuery({
    ...listErrorGroupsOptions({
      path: { project_id: project.id },
      query: { status: 'unresolved', page_size: 20, page: 1 },
    }),
  })

  const groups = (errorGroups as any)?.data || (errorGroups as any)?.items || errorGroups || []

  if (isLoading) {
    return (
      <div className="space-y-4">
        <Skeleton className="h-8 w-48" />
        <Skeleton className="h-64 w-full" />
      </div>
    )
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Wand2 className="h-5 w-5 text-muted-foreground" />
          <h1 className="text-xl font-semibold">Autofixer</h1>
        </div>
      </div>

      <p className="text-sm text-muted-foreground">
        Select an error to analyze and fix with AI. Claude will read your codebase, identify the root cause, and generate a fix.
      </p>

      {!project.git_provider_connection_id && (
        <Card>
          <CardContent className="p-6 text-center text-muted-foreground">
            <p>Connect a git provider with write access to use the Autofixer.</p>
          </CardContent>
        </Card>
      )}

      {project.git_provider_connection_id && groups.length === 0 && (
        <Card>
          <CardContent className="p-12 flex flex-col items-center justify-center text-center">
            <Sparkles className="h-12 w-12 text-muted-foreground mb-4" />
            <h2 className="text-lg font-semibold mb-2">No unresolved errors</h2>
            <p className="text-sm text-muted-foreground">
              When errors occur, they'll appear here and you can fix them with AI.
            </p>
          </CardContent>
        </Card>
      )}

      {project.git_provider_connection_id && groups.length > 0 && (
        <Card>
          <div className="overflow-x-auto">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Error</TableHead>
                  <TableHead className="hidden md:table-cell">Type</TableHead>
                  <TableHead className="hidden md:table-cell">Count</TableHead>
                  <TableHead className="hidden md:table-cell">Last seen</TableHead>
                  <TableHead className="w-[180px]">Autofix</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {groups.map((group: any) => (
                  <TableRow key={group.id}>
                    <TableCell className="max-w-[400px]">
                      <p className="font-medium truncate">{group.title}</p>
                    </TableCell>
                    <TableCell className="hidden md:table-cell">
                      <span className="text-xs bg-muted px-2 py-0.5 rounded">
                        {group.error_type}
                      </span>
                    </TableCell>
                    <TableCell className="hidden md:table-cell">
                      {group.total_count}
                    </TableCell>
                    <TableCell className="hidden md:table-cell text-muted-foreground text-sm">
                      {group.last_seen ? formatTimeAgo(group.last_seen) : '-'}
                    </TableCell>
                    <TableCell>
                      <Button
                        size="sm"
                        variant="outline"
                        onClick={() =>
                          navigate(`/projects/${project.slug}/errors/${group.id}/autofix`)
                        }
                      >
                        <Wand2 className="h-3 w-3 mr-1" />
                        Fix
                      </Button>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        </Card>
      )}
    </div>
  )
}
