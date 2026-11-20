// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listSurveys,
  createSurvey,
  getSurvey,
  updateSurvey,
  deleteSurvey,
  getSurveyResults,
  listSurveyResponses,
  getProject,
  getProjectBySlug,
  getEnvironments,
} from '../../api/sdk.gen.js'
import type {
  QuestionResults,
  SurveyQuestion,
  SurveyResponse,
  SurveyStatus,
  SurveyTargeting,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { promptConfirm } from '../../ui/prompts.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import { newline, header, icons, json, colors, success, info, keyValue } from '../../ui/output.js'
import { readJsonArgument } from '../flags/index.js'

const STATUSES: readonly SurveyStatus[] = ['draft', 'active', 'paused', 'completed']

export function registerSurveysCommands(program: Command): void {
  const surveys = program
    .command('surveys')
    .alias('survey')
    .description('Manage in-app surveys (NPS, ratings, choices, open text) and read their answers')

  surveys
    .command('list')
    .alias('ls')
    .description('List surveys')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--status <status>', 'Only surveys in this status: draft, active, paused, completed')
    .option('--page <n>', 'Page number (default: 1)')
    .option('--page-size <n>', 'Items per page (default: 20, max: 100)')
    .option('--json', 'Output in JSON format')
    .action(listSurveysCmd)

  surveys
    .command('get <id>')
    .description('Show a survey: its questions and when it is shown')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--json', 'Output in JSON format')
    .action(getSurveyCmd)

  surveys
    .command('create <name>')
    .description('Create a survey (a draft unless --status active)')
    .option('-p, --project <project>', 'Project slug or ID')
    .requiredOption(
      '-q, --questions <json|@file|->',
      'Questions as a JSON array of {id, kind, prompt, optional?, choices?, scale?, lower_label?, upper_label?}',
    )
    .option(
      '-t, --targeting <json|@file|->',
      'When to show it: {path_pattern?, trigger_event?, sample_rate?, once_per_visitor?, flag?: {key, value}}',
    )
    .option('--description <text>', 'Internal description')
    .option('--status <status>', 'draft (default) or active')
    .option('--json', 'Output in JSON format')
    .action(createSurveyCmd)

  surveys
    .command('update <id>')
    .description('Change a survey; only the options given are changed')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--name <name>', 'New name')
    .option('--description <text>', 'New description ("" clears it)')
    .option('-q, --questions <json|@file|->', 'Replace the questions')
    .option('-t, --targeting <json|@file|->', 'Replace when it is shown')
    .option('--status <status>', 'draft, active, paused or completed')
    .option('--json', 'Output in JSON format')
    .action(updateSurveyCmd)

  for (const [name, status, description] of [
    ['launch', 'active', 'Start showing a survey to visitors'],
    ['pause', 'paused', 'Stop showing a survey for now'],
    ['complete', 'completed', 'Stop showing a survey for good (its results stay)'],
  ] as const) {
    surveys
      .command(`${name} <id>`)
      .description(description)
      .option('-p, --project <project>', 'Project slug or ID')
      .action((id: string, options: { project?: string }) =>
        updateSurveyCmd(id, { project: options.project, status }),
      )
  }

  surveys
    .command('delete <id>')
    .description('Delete a survey (answers already recorded stay in analytics events)')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('-y, --yes', 'Skip the confirmation')
    .action(deleteSurveyCmd)

  surveys
    .command('results <id>')
    .description('Summarise the answers: counts, averages and NPS per question')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('-e, --environment <name>', 'Only answers from this environment')
    .option('--days <n>', 'Look back this many days (default: 30, max: 366)')
    .option('--json', 'Output in JSON format')
    .action(resultsCmd)

  surveys
    .command('responses <id>')
    .description('List individual responses, newest first')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('-e, --environment <name>', 'Only answers from this environment')
    .option('--days <n>', 'Look back this many days (default: 30, max: 366)')
    .option('--page <n>', 'Page number (default: 1)')
    .option('--page-size <n>', 'Items per page (default: 20, max: 100)')
    .option('--json', 'Output in JSON format')
    .action(responsesCmd)
}

// ============================================================================
// Helpers
// ============================================================================

export function parseSurveyId(raw: string): number {
  const id = Number(raw)
  if (!Number.isInteger(id) || id <= 0) {
    throw new Error(`Survey id must be a positive whole number, got "${raw}"`)
  }
  return id
}

export function assertStatus(raw: string): SurveyStatus {
  if ((STATUSES as readonly string[]).includes(raw)) return raw as SurveyStatus
  throw new Error(`Invalid status "${raw}". Must be one of: ${STATUSES.join(', ')}`)
}

/** `--days` into the results window the API takes. */
export function windowFromDays(
  raw: string | undefined,
  now: Date = new Date(),
): { start_date: string; end_date: string } | undefined {
  if (raw === undefined) return undefined
  const days = Number(raw)
  if (!Number.isInteger(days) || days < 1 || days > 366) {
    throw new Error(`--days must be a whole number from 1 to 366, got "${raw}"`)
  }
  const start = new Date(now.getTime() - days * 24 * 60 * 60 * 1000)
  return { start_date: start.toISOString(), end_date: now.toISOString() }
}

export async function readQuestions(raw: string): Promise<SurveyQuestion[]> {
  const value = await readJsonArgument(raw, '--questions')
  if (!Array.isArray(value)) {
    throw new Error('--questions must be a JSON array of questions')
  }
  return value as SurveyQuestion[]
}

export async function readTargeting(raw: string): Promise<SurveyTargeting> {
  const value = await readJsonArgument(raw, '--targeting')
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error('--targeting must be a JSON object')
  }
  return value as SurveyTargeting
}

/** One line describing when the survey shows, matching the console. */
export function describeTargeting(targeting: SurveyTargeting): string {
  const parts: string[] = [
    targeting.path_pattern ? `on ${targeting.path_pattern}` : 'on every page',
  ]
  if (targeting.trigger_event) parts.push(`after "${targeting.trigger_event}"`)
  const rate = targeting.sample_rate ?? 1
  if (rate < 1) parts.push(`${Math.round(rate * 1000) / 10}% of visitors`)
  if (targeting.flag) {
    parts.push(`where ${targeting.flag.key} = ${JSON.stringify(targeting.flag.value)}`)
  }
  if (targeting.once_per_visitor ?? true) parts.push('once per visitor')
  return parts.join(', ')
}

/** The lines `results` prints for one question. */
export function describeQuestionResults(question: QuestionResults): string[] {
  const lines = [`${question.answered} answered`]
  if (question.average !== null && question.average !== undefined) {
    lines[0] += `, average ${question.average.toFixed(1)}`
  }
  if (question.nps) {
    const score = question.nps.score === null || question.nps.score === undefined
      ? '-'
      : String(Math.round(question.nps.score))
    lines.push(
      `NPS ${score} (${question.nps.promoters} promoters, ${question.nps.passives} passives, ${question.nps.detractors} detractors)`,
    )
  }
  for (const row of question.distribution) {
    const share = question.answered > 0 ? Math.round((row.count / question.answered) * 100) : 0
    lines.push(`${row.value}: ${row.count} (${share}%)`)
  }
  if (question.other > 0) {
    lines.push(`${question.other} answers no longer match a choice`)
  }
  return lines
}

export function formatAnswer(value: unknown): string {
  if (Array.isArray(value)) return value.map(String).join(', ')
  if (value === null || value === undefined) return ''
  return String(value)
}

/** `--project` accepts a slug or a numeric id, as its help says. */
export function projectLookup(reference: string): { id: number } | { slug: string } {
  return /^\d+$/.test(reference) ? { id: Number.parseInt(reference, 10) } : { slug: reference }
}

async function resolveProject(projectOption?: string): Promise<{ slug: string; id: number }> {
  const resolved = await requireProjectSlug(projectOption)
  if (resolved.source !== 'flag') {
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  }
  // Only an explicit --project may be a numeric id; a slug stored in the
  // directory config, the environment or the context default is a slug.
  const lookup = resolved.source === 'flag' ? projectLookup(resolved.slug) : { slug: resolved.slug }
  const { data, error } =
    'id' in lookup
      ? await getProject({ client, path: { id: lookup.id } })
      : await getProjectBySlug({ client, path: { slug: lookup.slug } })
  if (error || !data) {
    throw new Error(`Project "${resolved.slug}" not found`)
  }
  return { slug: data.slug, id: data.id }
}

async function resolveEnvironmentId(projectId: number, nameOrSlug: string): Promise<number> {
  const { data, error } = await getEnvironments({ client, path: { project_id: projectId } })
  if (error || !data) throw new Error(getErrorMessage(error))
  const match = data.find((e) => e.name === nameOrSlug || e.slug === nameOrSlug)
  if (!match) {
    const available = data.map((e) => e.slug).join(', ')
    throw new Error(`Environment "${nameOrSlug}" not found. Available: ${available || '(none)'}`)
  }
  return match.id
}

function printSurvey(survey: SurveyResponse): void {
  newline()
  header(`${icons.folder} ${survey.name} (#${survey.id})`)
  keyValue('Status', survey.status)
  keyValue('Shown', describeTargeting(survey.targeting))
  keyValue('Description', survey.description ?? '-')
  keyValue('Updated', survey.updated_at)
  newline()
  if (survey.questions.length === 0) {
    info('No questions yet. Add some with: temps surveys update <id> --questions @questions.json')
  } else {
    printTable(
      survey.questions,
      [
        { header: 'Id', key: 'id', color: (v) => colors.bold(v) },
        { header: 'Kind', key: 'kind' },
        { header: 'Question', key: 'prompt' },
        {
          header: 'Options',
          accessor: (q) =>
            q.choices && q.choices.length > 0
              ? q.choices.join(' / ')
              : q.kind === 'rating'
                ? `1-${q.scale ?? 5}`
                : q.kind === 'nps'
                  ? '0-10'
                  : '-',
        },
        { header: 'Optional', accessor: (q) => (q.optional ? 'yes' : 'no') },
      ],
      { style: 'minimal' },
    )
  }
  newline()
}

// ============================================================================
// Commands
// ============================================================================

async function listSurveysCmd(options: {
  project?: string
  status?: string
  page?: string
  pageSize?: string
  json?: boolean
}): Promise<void> {
  await requireAuth()
  await setupClient()
  const status = options.status ? assertStatus(options.status) : undefined
  const project = await resolveProject(options.project)

  const data = await withSpinner('Fetching surveys...', async () => {
    const { data, error } = await listSurveys({
      client,
      path: { project_id: project.id },
      query: {
        ...(status ? { status } : {}),
        ...(options.page ? { page: Number(options.page) } : {}),
        ...(options.pageSize ? { page_size: Number(options.pageSize) } : {}),
      },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  if (options.json) {
    json(data ?? { surveys: [], total: 0, page: 1, page_size: 20, total_pages: 0 })
    return
  }

  const surveys = data?.surveys ?? []
  newline()
  header(`${icons.folder} Surveys for ${project.slug} (${data?.total ?? surveys.length})`)
  if (surveys.length === 0) {
    info(
      'No surveys yet. Create one with: temps surveys create "Checkout feedback" --questions @questions.json',
    )
    newline()
    return
  }
  const columns: TableColumn<SurveyResponse>[] = [
    { header: 'Id', accessor: (s) => String(s.id) },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Status', key: 'status' },
    { header: 'Questions', accessor: (s) => String(s.questions.length) },
    { header: 'Shown', accessor: (s) => describeTargeting(s.targeting) },
  ]
  printTable(surveys, columns, { style: 'minimal' })
  const total = data?.total ?? surveys.length
  if (total > surveys.length) {
    info(
      `Showing ${surveys.length} of ${total} (page ${data?.page ?? 1} of ${data?.total_pages ?? 1}). Use --page to see more.`,
    )
  }
  newline()
}

async function fetchSurvey(projectId: number, surveyId: number): Promise<SurveyResponse> {
  const { data, error } = await getSurvey({
    client,
    path: { project_id: projectId, survey_id: surveyId },
  })
  if (error) throw new Error(getErrorMessage(error))
  if (!data) throw new Error(`Survey ${surveyId} not found`)
  return data
}

async function getSurveyCmd(
  rawId: string,
  options: { project?: string; json?: boolean },
): Promise<void> {
  await requireAuth()
  await setupClient()
  const surveyId = parseSurveyId(rawId)
  const project = await resolveProject(options.project)
  const survey = await withSpinner(`Fetching survey ${surveyId}...`, () =>
    fetchSurvey(project.id, surveyId),
  )
  if (options.json) {
    json(survey)
    return
  }
  printSurvey(survey)
}

async function createSurveyCmd(
  name: string,
  options: {
    project?: string
    questions: string
    targeting?: string
    description?: string
    status?: string
    json?: boolean
  },
): Promise<void> {
  await requireAuth()
  await setupClient()
  const status = options.status ? assertStatus(options.status) : 'draft'
  const questions = await readQuestions(options.questions)
  const targeting = options.targeting ? await readTargeting(options.targeting) : undefined
  const project = await resolveProject(options.project)

  const survey = await withSpinner(`Creating ${name}...`, async () => {
    const { data, error } = await createSurvey({
      client,
      path: { project_id: project.id },
      body: {
        name,
        description: options.description ?? null,
        status,
        questions,
        ...(targeting ? { targeting } : {}),
      },
    })
    if (error) throw new Error(getErrorMessage(error))
    if (!data) throw new Error(`Creating survey "${name}" returned no survey`)
    return data
  })

  if (options.json) {
    json(survey)
    return
  }
  newline()
  success(`Created survey ${colors.bold(survey.name)} (#${survey.id}) as ${survey.status}`)
  if (survey.status === 'draft') {
    info(`Visitors see it once you run: temps surveys launch ${survey.id}`)
  }
  newline()
}

async function updateSurveyCmd(
  rawId: string,
  options: {
    project?: string
    name?: string
    description?: string
    questions?: string
    targeting?: string
    status?: string
    json?: boolean
  },
): Promise<void> {
  await requireAuth()
  await setupClient()
  const surveyId = parseSurveyId(rawId)
  const status = options.status ? assertStatus(options.status) : undefined
  const questions = options.questions ? await readQuestions(options.questions) : undefined
  const targeting = options.targeting ? await readTargeting(options.targeting) : undefined
  if (
    options.name === undefined &&
    options.description === undefined &&
    !questions &&
    !targeting &&
    !status
  ) {
    throw new Error(
      'Nothing to change. Pass --name, --description, --questions, --targeting or --status.',
    )
  }
  const project = await resolveProject(options.project)

  const survey = await withSpinner(`Updating survey ${surveyId}...`, async () => {
    // The revision guard: a change someone else saved in between is refused
    // instead of being silently overwritten.
    const current = await fetchSurvey(project.id, surveyId)
    const { data, error } = await updateSurvey({
      client,
      path: { project_id: project.id, survey_id: surveyId },
      body: {
        expected_updated_at: current.updated_at,
        ...(options.name !== undefined ? { name: options.name } : {}),
        ...(options.description !== undefined
          ? { description: options.description === '' ? null : options.description }
          : {}),
        ...(questions ? { questions } : {}),
        ...(targeting ? { targeting } : {}),
        ...(status ? { status } : {}),
      },
    })
    if (error) throw new Error(getErrorMessage(error))
    if (!data) throw new Error(`Updating survey ${surveyId} returned no survey`)
    return data
  })

  if (options.json) {
    json(survey)
    return
  }
  newline()
  success(`Saved ${colors.bold(survey.name)} (#${survey.id}), now ${survey.status}`)
  newline()
}

async function deleteSurveyCmd(
  rawId: string,
  options: { project?: string; yes?: boolean },
): Promise<void> {
  await requireAuth()
  await setupClient()
  const surveyId = parseSurveyId(rawId)
  if (!options.yes) {
    const confirmed = await promptConfirm({
      message: `Delete survey ${surveyId}? Visitors stop seeing it; answers already recorded stay in analytics events.`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }
  const project = await resolveProject(options.project)
  await withSpinner(`Deleting survey ${surveyId}...`, async () => {
    const { error } = await deleteSurvey({
      client,
      path: { project_id: project.id, survey_id: surveyId },
    })
    if (error) throw new Error(getErrorMessage(error))
  })
  newline()
  success(`Deleted survey ${surveyId}`)
  newline()
}

async function resultsCmd(
  rawId: string,
  options: { project?: string; environment?: string; days?: string; json?: boolean },
): Promise<void> {
  await requireAuth()
  await setupClient()
  const surveyId = parseSurveyId(rawId)
  const window = windowFromDays(options.days)
  const project = await resolveProject(options.project)

  const results = await withSpinner('Fetching results...', async () => {
    const environmentId = options.environment
      ? await resolveEnvironmentId(project.id, options.environment)
      : undefined
    const { data, error } = await getSurveyResults({
      client,
      path: { project_id: project.id, survey_id: surveyId },
      query: { ...(window ?? {}), ...(environmentId ? { environment_id: environmentId } : {}) },
    })
    if (error) throw new Error(getErrorMessage(error))
    if (!data) throw new Error(`Survey ${surveyId} returned no results`)
    return data
  })

  if (options.json) {
    json(results)
    return
  }
  newline()
  header(`${icons.folder} Results for survey ${surveyId}`)
  keyValue('Window', `${results.start_date} to ${results.end_date}`)
  keyValue('Shown', String(results.shown))
  keyValue('Answered', String(results.sent))
  keyValue('Dismissed', String(results.dismissed))
  keyValue(
    'Response rate',
    results.response_rate === null || results.response_rate === undefined
      ? '-'
      : `${Math.round(results.response_rate * 100)}%`,
  )
  for (const question of results.questions) {
    newline()
    header(question.prompt)
    for (const line of describeQuestionResults(question)) info(line)
  }
  newline()
}

async function responsesCmd(
  rawId: string,
  options: {
    project?: string
    environment?: string
    days?: string
    page?: string
    pageSize?: string
    json?: boolean
  },
): Promise<void> {
  await requireAuth()
  await setupClient()
  const surveyId = parseSurveyId(rawId)
  const window = windowFromDays(options.days)
  const project = await resolveProject(options.project)

  const data = await withSpinner('Fetching responses...', async () => {
    const environmentId = options.environment
      ? await resolveEnvironmentId(project.id, options.environment)
      : undefined
    const { data, error } = await listSurveyResponses({
      client,
      path: { project_id: project.id, survey_id: surveyId },
      query: {
        ...(window ?? {}),
        ...(environmentId ? { environment_id: environmentId } : {}),
        ...(options.page ? { page: Number(options.page) } : {}),
        ...(options.pageSize ? { page_size: Number(options.pageSize) } : {}),
      },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  if (options.json) {
    json(data ?? { responses: [], total: 0, page: 1, page_size: 20, total_pages: 0 })
    return
  }
  const responses = data?.responses ?? []
  newline()
  header(`${icons.folder} Responses to survey ${surveyId} (${data?.total ?? responses.length})`)
  if (responses.length === 0) {
    info('No responses in this window.')
    newline()
    return
  }
  printTable(
    responses,
    [
      { header: 'When', key: 'timestamp' },
      {
        header: 'Answers',
        accessor: (r) =>
          Object.entries(r.answers)
            .map(([id, value]) => `${id}=${formatAnswer(value)}`)
            .join('; '),
      },
      { header: 'Page', key: 'pathname' },
      { header: 'Visitor', accessor: (r) => (r.visitor_id ? String(r.visitor_id) : '-') },
    ],
    { style: 'minimal' },
  )
  const total = data?.total ?? responses.length
  if (total > responses.length) {
    info(
      `Showing ${responses.length} of ${total} (page ${data?.page ?? 1} of ${data?.total_pages ?? 1}). Use --page to see more.`,
    )
  }
  newline()
}
