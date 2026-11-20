// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProjectResponse, SurveyResponse } from '@/api/client'
import { listSurveysOptions } from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { CreateActionButton } from '@/components/ui/create-action-button'
import { EmptyPlaceholder } from '@/components/ui/empty-placeholder'
import { ReadFailure } from '@/components/ui/read-failure'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
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
import { MessageSquareText, Plus } from 'lucide-react'
import { useState } from 'react'
import { Route, Routes, useNavigate } from 'react-router'
import { SurveyDetail } from './SurveyDetail'
import { SurveyEditorDialog } from './SurveyEditorDialog'
import { describeTargeting, STATUS_LABELS, statusVariant } from './survey-form'

interface ProjectSurveysProps {
  project: ProjectResponse
}

/** `analytics/surveys/*`: the list, and one survey's results. */
export function ProjectSurveys({ project }: ProjectSurveysProps) {
  return (
    <Routes>
      <Route index element={<SurveyList project={project} />} />
      <Route path=":surveyId" element={<SurveyDetail project={project} />} />
    </Routes>
  )
}

function SurveyList({ project }: ProjectSurveysProps) {
  const navigate = useNavigate()
  const [page, setPage] = useState(1)
  const [createOpen, setCreateOpen] = useState(false)
  const surveysQuery = useQuery({
    ...listSurveysOptions({
      path: { project_id: project.id },
      query: { page },
    }),
  })
  const surveys = surveysQuery.data?.surveys ?? []
  const open = (survey: SurveyResponse) =>
    navigate(`/projects/${project.slug}/analytics/surveys/${survey.id}`)

  return (
    <div className="space-y-6">
      {/* What surveys are for is explained by the empty state; a populated
          list shows only its data and actions. */}
      <div className="flex justify-end">
        <CreateActionButton
          className="w-full sm:w-auto"
          onClick={() => setCreateOpen(true)}
          label="New survey"
          icon={<Plus className="mr-1.5 h-4 w-4" />}
        />
      </div>

      {surveysQuery.isError && (
        <ReadFailure
          resource="Surveys"
          error={surveysQuery.error}
          cached={surveys.length > 0}
          onRetry={() => void surveysQuery.refetch()}
          retrying={surveysQuery.isFetching}
        />
      )}

      {surveysQuery.isPending ? (
        <div className="space-y-2">
          {[0, 1, 2].map((i) => (
            <Skeleton key={i} className="h-12 w-full" />
          ))}
        </div>
      ) : surveys.length === 0 && !surveysQuery.isError ? (
        <EmptyPlaceholder
          className="min-h-[280px] rounded-md border"
          icon={MessageSquareText}
          title="No surveys yet"
          description='For example, "How likely are you to recommend us?" after checkout. Add <TempsSurveys /> or set surveys: { enabled: true } in the analytics SDK and active surveys appear in your app.'
          action={
            <CreateActionButton
              onClick={() => setCreateOpen(true)}
              label="Create a survey"
              hideShortcut
              icon={<Plus className="mr-1.5 h-4 w-4" />}
            />
          }
        />
      ) : (
        surveys.length > 0 && (
          <div className="space-y-4">
            {/* Columns show by the table's own width (DESIGN.md). */}
            <div className="@container">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Name</TableHead>
                    <TableHead>Status</TableHead>
                    <TableHead className="hidden @2xl:table-cell">
                      When it shows
                    </TableHead>
                    <TableHead className="hidden @xl:table-cell text-right">
                      Questions
                    </TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {surveys.map((survey) => (
                    <TableRow
                      key={survey.id}
                      className="cursor-pointer"
                      tabIndex={0}
                      onClick={() => open(survey)}
                      onKeyDown={(e) => {
                        if (e.key === 'Enter') open(survey)
                      }}
                    >
                      <TableCell className="font-medium">
                        {survey.name}
                      </TableCell>
                      <TableCell>
                        <Badge variant={statusVariant(survey.status)}>
                          {STATUS_LABELS[survey.status]}
                        </Badge>
                      </TableCell>
                      <TableCell className="hidden @2xl:table-cell text-sm text-muted-foreground">
                        {describeTargeting(survey.targeting)}
                      </TableCell>
                      <TableCell className="hidden @xl:table-cell text-right tabular-nums">
                        {survey.questions.length}
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
            <ResponsivePagination
              page={page}
              pageSize={surveysQuery.data?.page_size ?? 20}
              total={surveysQuery.data?.total ?? 0}
              totalPages={surveysQuery.data?.total_pages ?? 1}
              onPageChange={setPage}
            />
          </div>
        )
      )}

      <SurveyEditorDialog
        projectId={project.id}
        open={createOpen}
        onOpenChange={setCreateOpen}
        onSaved={(survey) => {
          void surveysQuery.refetch()
          open(survey)
        }}
      />
    </div>
  )
}
