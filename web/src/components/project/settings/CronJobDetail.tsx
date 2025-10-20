import {
  getCronByIdOptions,
  getCronExecutionsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse } from '@/api/client'
import { useQuery } from '@tanstack/react-query'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Clock, ArrowLeft, CheckCircle2, XCircle } from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useNavigate, useParams } from 'react-router-dom'
import { format } from 'date-fns'

interface CronJobDetailProps {
  project: ProjectResponse
}

export function CronJobDetail({ project }: CronJobDetailProps) {
  const navigate = useNavigate()
  const { environmentId, cronId } = useParams<{
    environmentId: string
    cronId: string
  }>()
  const { data: cronJob, isLoading: isLoadingCron } = useQuery({
    ...getCronByIdOptions({
      path: {
        project_id: project.id,
        env_id: Number(environmentId),
        cron_id: Number(cronId),
      },
    }),
  })

  const { data: executions, isLoading: isLoadingExecutions } = useQuery({
    ...getCronExecutionsOptions({
      path: {
        project_id: project.id,
        env_id: Number(environmentId),
        cron_id: Number(cronId),
      },
      query: {
        page: 1,
        per_page: 10,
      },
    }),
  })

  const isLoading = isLoadingCron || isLoadingExecutions

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-4">
        <Button variant="ghost" size="icon" onClick={() => navigate(-1)}>
          <ArrowLeft className="h-4 w-4" />
        </Button>
        <div>
          <h2 className="text-lg font-medium">Cron Job Details</h2>
          <p className="text-sm text-muted-foreground">
            View cron job configuration and execution history
          </p>
        </div>
      </div>

      {isLoading ? (
        <div className="space-y-4">
          <Card className="animate-pulse">
            <CardContent className="h-32" />
          </Card>
          <Card className="animate-pulse">
            <CardContent className="h-64" />
          </Card>
        </div>
      ) : (
        <div className="space-y-6">
          {/* Cron Job Details */}
          <Card>
            <CardHeader>
              <CardTitle>Configuration</CardTitle>
            </CardHeader>
            <CardContent className="space-y-4">
              <div className="grid gap-4 md:grid-cols-2">
                <div>
                  <div className="text-sm font-medium">Path</div>
                  <code className="text-sm">{cronJob?.path}</code>
                </div>
                <div>
                  <div className="text-sm font-medium">Schedule</div>
                  <code className="text-sm">{cronJob?.schedule}</code>
                </div>
                <div>
                  <div className="text-sm font-medium">Next Run</div>
                  <div className="text-sm text-muted-foreground">
                    {cronJob?.next_run
                      ? format(new Date(cronJob.next_run), 'PPpp')
                      : 'Not scheduled'}
                  </div>
                </div>
                <div>
                  <div className="text-sm font-medium">Created</div>
                  <div className="text-sm text-muted-foreground">
                    {format(new Date(cronJob?.created_at || ''), 'PPpp')}
                  </div>
                </div>
              </div>
            </CardContent>
          </Card>

          {/* Execution History */}
          <Card>
            <CardHeader>
              <CardTitle>Recent Executions</CardTitle>
              <CardDescription>
                Last 10 executions of this cron job
              </CardDescription>
            </CardHeader>
            <CardContent>
              {!executions?.length ? (
                <div className="flex flex-col items-center justify-center py-8 text-center">
                  <Clock className="h-8 w-8 text-muted-foreground mb-4" />
                  <p className="text-sm text-muted-foreground">
                    No executions yet
                  </p>
                </div>
              ) : (
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Time</TableHead>
                      <TableHead>Status</TableHead>
                      <TableHead>Response Time</TableHead>
                      <TableHead>Details</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {executions.map((execution) => (
                      <TableRow key={execution.id}>
                        <TableCell>
                          {format(new Date(execution.executed_at), 'PPpp')}
                        </TableCell>
                        <TableCell>
                          <div className="flex items-center gap-2">
                            {execution.status_code >= 200 &&
                            execution.status_code < 300 ? (
                              <>
                                <CheckCircle2 className="h-4 w-4 text-green-500" />
                                <span className="text-sm">Success</span>
                              </>
                            ) : (
                              <>
                                <XCircle className="h-4 w-4 text-destructive" />
                                <span className="text-sm">Failed</span>
                              </>
                            )}
                          </div>
                        </TableCell>
                        <TableCell>{execution.response_time_ms}ms</TableCell>
                        <TableCell>
                          <div className="space-y-1">
                            <div className="text-sm">
                              Status: {execution.status_code}
                            </div>
                            {execution.error_message && (
                              <div className="text-sm text-destructive">
                                {execution.error_message}
                              </div>
                            )}
                          </div>
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              )}
            </CardContent>
          </Card>
        </div>
      )}
    </div>
  )
}
