// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { getFailureReportPreview, sendFailureReport } from '../../api/sdk.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { newline, success, info, warning, keyValue, colors } from '../../ui/output.js'
import { parseDeploymentTarget } from './actions.js'

interface FailureReportOptions {
  projectId: string
  deploymentId: string
  jobId: string
}

interface FailureReportSendOptions extends FailureReportOptions {
  textFile?: string
}

/** Read stdin fully when it's piped; returns undefined for an interactive TTY. */
async function readStdin(): Promise<string | undefined> {
  if (process.stdin.isTTY) {
    return undefined
  }
  const chunks: Buffer[] = []
  for await (const chunk of process.stdin) {
    chunks.push(Buffer.from(chunk))
  }
  const text = Buffer.concat(chunks).toString('utf8').trim()
  return text.length > 0 ? text : undefined
}

export async function failureReportPreviewAction(
  options: FailureReportOptions,
): Promise<void> {
  await requireAuth()
  await setupClient()

  const target = parseDeploymentTarget(options)
  if (!target) {
    warning('Invalid project or deployment ID')
    return
  }
  const { projectId: projId, deploymentId: deplId } = target

  const preview = await withSpinner('Building failure-report preview...', async () => {
    const { data, error } = await getFailureReportPreview({
      client,
      path: { project_id: projId, deployment_id: deplId, job_id: options.jobId },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? 'Failed to build failure-report preview')
    }
    return data
  })

  newline()
  keyValue('Failed job type', preview.failed_job_type)
  if (preview.error_message) {
    keyValue('Error', preview.error_message)
  }
  keyValue('Reporting to Temps enabled', preview.reporting_enabled ? 'yes' : 'no')
  newline()
  info('Redacted log (review before sending — see "deployments failure-report send"):')
  newline()
  console.log(colors.muted(preview.redacted_log))
}

export async function failureReportSendAction(
  options: FailureReportSendOptions,
): Promise<void> {
  await requireAuth()
  await setupClient()

  const target = parseDeploymentTarget(options)
  if (!target) {
    warning('Invalid project or deployment ID')
    return
  }
  const { projectId: projId, deploymentId: deplId } = target

  let reportText: string | undefined
  if (options.textFile) {
    const fs = await import('node:fs/promises')
    reportText = (await fs.readFile(options.textFile, 'utf8')).trim()
  } else {
    reportText = await readStdin()
  }

  if (!reportText) {
    reportText = await withSpinner('Fetching redacted preview to send...', async () => {
      const { data, error } = await getFailureReportPreview({
        client,
        path: { project_id: projId, deployment_id: deplId, job_id: options.jobId },
      })
      if (error || !data) {
        throw new Error(getErrorMessage(error) ?? 'Failed to build failure-report preview')
      }
      return data.redacted_log
    })
  }

  await withSpinner('Sending failure report to the Temps team...', async () => {
    const { error } = await sendFailureReport({
      client,
      path: { project_id: projId, deployment_id: deplId, job_id: options.jobId },
      body: { report_text: reportText! },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('Failure report sent')
}
