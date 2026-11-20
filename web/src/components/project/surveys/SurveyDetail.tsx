// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  ProjectResponse,
  QuestionResults,
  SurveyResponse,
  SurveyStatus,
} from '@/api/client'
import {
  deleteSurveyMutation,
  getSurveyOptions,
  getSurveyResultsOptions,
  listSurveyResponsesOptions,
  updateSurveyMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { ReadFailure } from '@/components/ui/read-failure'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useGoBack } from '@/hooks/useGoBack'
import { useMutation, useQuery } from '@tanstack/react-query'
import { ArrowLeft, Loader2, Pencil, Trash2 } from 'lucide-react'
import { useMemo, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router'
import { toast } from 'sonner'
import { SurveyEditorDialog } from './SurveyEditorDialog'
import {
  describeTargeting,
  formatAnswer,
  percentOf,
  STATUS_LABELS,
  statusActions,
  statusVariant,
  surveyErrorMessage,
} from './survey-form'

const WINDOWS = [7, 30, 90] as const

interface SurveyDetailProps {
  project: ProjectResponse
}

export function SurveyDetail({ project }: SurveyDetailProps) {
  const { surveyId: rawId } = useParams()
  const surveyId = Number(rawId)
  const listPath = `/projects/${project.slug}/analytics/surveys`
  const goBack = useGoBack(listPath)
  const navigate = useNavigate()
  const [days, setDays] = useState<(typeof WINDOWS)[number]>(30)
  const [page, setPage] = useState(1)
  const [editOpen, setEditOpen] = useState(false)
  const [deleteOpen, setDeleteOpen] = useState(false)
  const path = { project_id: project.id, survey_id: surveyId }

  // Fixed per chosen window so the query key does not change every render.
  const window = useMemo(() => {
    const end = new Date()
    const start = new Date(end.getTime() - days * 24 * 60 * 60 * 1000)
    return { start_date: start.toISOString(), end_date: end.toISOString() }
  }, [days])

  const surveyQuery = useQuery({
    ...getSurveyOptions({ path }),
    enabled: surveyId > 0,
  })
  const resultsQuery = useQuery({
    ...getSurveyResultsOptions({ path, query: window }),
    enabled: surveyId > 0,
  })
  const responsesQuery = useQuery({
    ...listSurveyResponsesOptions({ path, query: { ...window, page } }),
    enabled: surveyId > 0,
  })

  const changeStatus = useMutation({
    ...updateSurveyMutation(),
    onSuccess: (saved) => {
      toast.success(
        `${saved.name} is now ${STATUS_LABELS[saved.status].toLowerCase()}`
      )
      void surveyQuery.refetch()
    },
    onError: (error) =>
      toast.error(surveyErrorMessage(error, 'Could not change the status')),
  })
  const remove = useMutation({
    ...deleteSurveyMutation(),
    onSuccess: () => {
      toast.success(
        'Survey deleted. Its answers stay in your analytics events.'
      )
      navigate(listPath)
    },
    onError: (error) =>
      toast.error(surveyErrorMessage(error, 'Could not delete the survey')),
  })

  const survey = surveyQuery.data
  if (surveyQuery.isPending && surveyId > 0) {
    return (
      <div className="space-y-4">
        <Skeleton className="h-10 w-1/2" />
        <Skeleton className="h-24 w-full" />
        <Skeleton className="h-64 w-full" />
      </div>
    )
  }
  if (!survey) {
    return (
      <ReadFailure
        resource="Survey"
        error={
          surveyQuery.error ?? new Error(`Survey ${rawId ?? ''} was not found`)
        }
        onRetry={
          surveyQuery.isError ? () => void surveyQuery.refetch() : undefined
        }
        retrying={surveyQuery.isFetching}
      />
    )
  }

  const setStatus = (to: SurveyStatus) =>
    changeStatus.mutate({
      path,
      body: { status: to, expected_updated_at: survey.updated_at },
    })

  return (
    <div className="space-y-6">
      <div className="flex flex-col gap-4 lg:flex-row lg:items-start lg:justify-between">
        <div className="space-y-1">
          <Button variant="ghost" size="sm" className="-ml-2" onClick={goBack}>
            <ArrowLeft className="mr-1.5 h-4 w-4" />
            Surveys
          </Button>
          <div className="flex flex-wrap items-center gap-2">
            <h2 className="text-xl font-semibold">{survey.name}</h2>
            <Badge variant={statusVariant(survey.status)}>
              {STATUS_LABELS[survey.status]}
            </Badge>
          </div>
          <p className="text-sm text-muted-foreground">
            {describeTargeting(survey.targeting)}
          </p>
          {survey.description && (
            <p className="text-sm">{survey.description}</p>
          )}
        </div>
        <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap">
          {statusActions(survey.status).map((action) => (
            <Button
              key={action.to}
              variant={action.to === 'active' ? 'default' : 'outline'}
              disabled={changeStatus.isPending}
              onClick={() => setStatus(action.to)}
            >
              {changeStatus.isPending &&
                changeStatus.variables?.body.status === action.to && (
                  <Loader2 className="mr-1.5 h-4 w-4 animate-spin" />
                )}
              {action.label}
            </Button>
          ))}
          <Button variant="outline" onClick={() => setEditOpen(true)}>
            <Pencil className="mr-1.5 h-4 w-4" />
            Edit
          </Button>
          <Button
            variant="outline"
            onClick={() => setDeleteOpen(true)}
            aria-label="Delete survey"
          >
            <Trash2 className="h-4 w-4 sm:mr-1.5" />
            <span className="max-sm:sr-only">Delete</span>
          </Button>
        </div>
      </div>

      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <h3 className="text-sm font-medium">Results</h3>
        <Select
          value={String(days)}
          onValueChange={(v) => {
            setDays(Number(v) as (typeof WINDOWS)[number])
            setPage(1)
          }}
        >
          <SelectTrigger
            className="w-full sm:w-[160px]"
            aria-label="Results window"
          >
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {WINDOWS.map((d) => (
              <SelectItem key={d} value={String(d)}>
                Last {d} days
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      {resultsQuery.isError ? (
        <ReadFailure
          resource="Survey results"
          error={resultsQuery.error}
          onRetry={() => void resultsQuery.refetch()}
          retrying={resultsQuery.isFetching}
        />
      ) : resultsQuery.isPending ? (
        <div className="grid grid-cols-2 gap-4 md:grid-cols-4">
          {[0, 1, 2, 3].map((i) => (
            <Skeleton key={i} className="h-20" />
          ))}
        </div>
      ) : (
        <>
          <div className="grid grid-cols-2 gap-4 md:grid-cols-4">
            <Stat
              label="Shown"
              value={resultsQuery.data.shown.toLocaleString()}
            />
            <Stat
              label="Answered"
              value={resultsQuery.data.sent.toLocaleString()}
            />
            <Stat
              label="Dismissed"
              value={resultsQuery.data.dismissed.toLocaleString()}
            />
            <Stat
              label="Response rate"
              value={
                resultsQuery.data.response_rate == null
                  ? '—'
                  : `${Math.round(resultsQuery.data.response_rate * 100)}%`
              }
            />
          </div>
          {resultsQuery.data.shown === 0 && <NotShownYet survey={survey} />}
          <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
            {resultsQuery.data.questions.map((question) => (
              <QuestionCard key={question.question_id} question={question} />
            ))}
          </div>
        </>
      )}

      <section className="space-y-3" aria-labelledby="survey-responses-heading">
        <h3 id="survey-responses-heading" className="text-sm font-medium">
          Responses
        </h3>
        {responsesQuery.isError ? (
          <ReadFailure
            resource="Survey responses"
            error={responsesQuery.error}
            onRetry={() => void responsesQuery.refetch()}
            retrying={responsesQuery.isFetching}
          />
        ) : responsesQuery.isPending ? (
          <Skeleton className="h-40 w-full" />
        ) : responsesQuery.data.responses.length === 0 ? (
          <p className="rounded-md border p-4 text-sm text-muted-foreground">
            No answers in the last {days} days.
          </p>
        ) : (
          <div className="space-y-4">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>When</TableHead>
                  <TableHead>Answers</TableHead>
                  <TableHead className="hidden md:table-cell">Page</TableHead>
                  <TableHead className="text-right">Visitor</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {responsesQuery.data.responses.map((response) => (
                  <TableRow key={response.event_id}>
                    <TableCell className="whitespace-nowrap text-sm">
                      <TimeAgo date={response.timestamp} />
                    </TableCell>
                    <TableCell className="text-sm">
                      <dl className="space-y-0.5">
                        {survey.questions
                          .filter((q) => response.answers[q.id] !== undefined)
                          .map((q) => (
                            <div key={q.id} className="flex gap-2">
                              <dt className="shrink-0 text-muted-foreground">
                                {q.prompt}
                              </dt>
                              <dd className="break-words">
                                {formatAnswer(response.answers[q.id])}
                              </dd>
                            </div>
                          ))}
                      </dl>
                    </TableCell>
                    <TableCell className="hidden md:table-cell font-mono text-xs">
                      {response.pathname}
                    </TableCell>
                    <TableCell className="text-right">
                      {response.visitor_id ? (
                        <Link
                          className="text-sm underline-offset-4 hover:underline"
                          to={`/projects/${project.slug}/analytics/visitors/${response.visitor_id}`}
                        >
                          Profile
                        </Link>
                      ) : (
                        <span className="text-sm text-muted-foreground">
                          Anonymous
                        </span>
                      )}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
            <ResponsivePagination
              page={page}
              pageSize={responsesQuery.data.page_size}
              total={responsesQuery.data.total}
              totalPages={responsesQuery.data.total_pages}
              onPageChange={setPage}
            />
          </div>
        )}
      </section>

      <SurveyEditorDialog
        projectId={project.id}
        survey={survey}
        open={editOpen}
        onOpenChange={setEditOpen}
        onSaved={() => {
          void surveyQuery.refetch()
          void resultsQuery.refetch()
        }}
      />
      <ConfirmDeleteDialog
        open={deleteOpen}
        onOpenChange={setDeleteOpen}
        resourceType="survey"
        resourceName={survey.name}
        consequences="Visitors stop seeing it. The answers already recorded stay in your analytics events, but this page goes away."
        isPending={remove.isPending}
        onConfirm={() => remove.mutate({ path })}
      />
    </div>
  )
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <Card>
      <CardContent className="p-4">
        <p className="text-xs text-muted-foreground">{label}</p>
        <p className="text-2xl font-semibold tabular-nums">{value}</p>
      </CardContent>
    </Card>
  )
}

function NotShownYet({ survey }: { survey: SurveyResponse }) {
  const message =
    survey.status === 'active'
      ? 'Nobody has seen this survey in this window. Check that the app renders <TempsSurveys /> (React) or enables surveys: {} in the analytics SDK, and that the path, event and flag above match.'
      : `This survey is ${STATUS_LABELS[survey.status].toLowerCase()}, so visitors do not see it. Launch it to start collecting answers.`
  return (
    <p className="rounded-md border border-dashed p-4 text-sm text-muted-foreground">
      {message}
    </p>
  )
}

function QuestionCard({ question }: { question: QuestionResults }) {
  const rows = question.distribution
  const max = rows.reduce((m, r) => Math.max(m, r.count), 0)
  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-base">{question.prompt}</CardTitle>
        <p className="text-xs text-muted-foreground">
          {question.answered.toLocaleString()} answered
          {question.average != null &&
            ` · average ${question.average.toFixed(1)}`}
        </p>
      </CardHeader>
      <CardContent className="space-y-3">
        {question.nps && (
          <div className="flex flex-wrap items-baseline gap-x-4 gap-y-1">
            <span className="text-3xl font-semibold tabular-nums">
              {question.nps.score == null
                ? '—'
                : Math.round(question.nps.score)}
            </span>
            <span className="text-sm text-muted-foreground">NPS</span>
            <span className="text-xs text-muted-foreground">
              {question.nps.promoters} promoters · {question.nps.passives}{' '}
              passives · {question.nps.detractors} detractors
            </span>
          </div>
        )}
        {question.kind === 'open_text' ? (
          <p className="text-sm text-muted-foreground">
            Written answers are listed under Responses.
          </p>
        ) : rows.length === 0 ? (
          <p className="text-sm text-muted-foreground">No answers yet.</p>
        ) : (
          <ul className="space-y-1.5">
            {rows.map((row) => (
              <li
                key={row.value}
                className="grid grid-cols-[minmax(0,8rem)_1fr_auto] items-center gap-2 text-sm"
              >
                <span className="truncate" title={row.value}>
                  {row.value}
                </span>
                <div className="relative h-1.5 w-full overflow-hidden rounded-full bg-muted">
                  <div
                    className="absolute inset-y-0 left-0 rounded-full bg-primary"
                    style={{ width: `${percentOf(row.count, max)}%` }}
                  />
                </div>
                <span className="text-xs tabular-nums text-muted-foreground">
                  {row.count.toLocaleString()} (
                  {percentOf(row.count, question.answered)}%)
                </span>
              </li>
            ))}
          </ul>
        )}
        {question.other > 0 && (
          <p className="text-xs text-muted-foreground">
            {question.other.toLocaleString()} answers did not match a current
            choice (the question changed after they were given).
          </p>
        )}
      </CardContent>
    </Card>
  )
}
