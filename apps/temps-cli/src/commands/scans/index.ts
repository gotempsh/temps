// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listProjectScans,
  triggerScan,
  getLatestScan,
  getLatestScansPerEnvironment,
  getScan,
  getScanVulnerabilities,
  deleteScan,
  getScanByDeployment,
} from '../../api/sdk.gen.js'
import type { ScanResponse, VulnerabilityResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptConfirm } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning, keyValue, formatRelativeTime } from '../../ui/output.js'

interface ListOptions {
  projectId: string
  page?: string
  pageSize?: string
  json?: boolean
}

interface TriggerOptions {
  projectId: string
  environmentId: string
}

interface LatestOptions {
  projectId: string
  environmentId?: string
  json?: boolean
}

interface EnvironmentsOptions {
  projectId: string
  json?: boolean
}

interface ShowOptions {
  id: string
  json?: boolean
}

interface VulnerabilitiesOptions {
  id: string
  severity?: string
  json?: boolean
}

interface RemoveOptions {
  id: string
  force?: boolean
  yes?: boolean
}

interface ByDeploymentOptions {
  deploymentId: string
  json?: boolean
}

export function registerScansCommands(program: Command): void {
  const scans = program
    .command('scans')
    .alias('scan')
    .description('Manage vulnerability scans')

  scans
    .command('list')
    .alias('ls')
    .description('List vulnerability scans for a project')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--page <n>', 'Page number')
    .option('--page-size <n>', 'Items per page (default: 20, max: 100)')
    .option('--json', 'Output in JSON format')
    .action(listScans)

  scans
    .command('trigger')
    .description('Trigger a new vulnerability scan')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--environment-id <id>', 'Environment ID to scan')
    .action(triggerScanAction)

  scans
    .command('latest')
    .description('Get the latest scan for a project')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--environment-id <id>', 'Filter by environment ID')
    .option('--json', 'Output in JSON format')
    .action(latestScan)

  scans
    .command('environments')
    .alias('envs')
    .description('Get latest scans per environment')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--json', 'Output in JSON format')
    .action(environmentsScans)

  scans
    .command('show')
    .description('Show scan details')
    .requiredOption('--id <id>', 'Scan ID')
    .option('--json', 'Output in JSON format')
    .action(showScan)

  scans
    .command('vulnerabilities')
    .alias('vulns')
    .description('List vulnerabilities found in a scan')
    .requiredOption('--id <id>', 'Scan ID')
    .option('--severity <level>', 'Filter by severity (CRITICAL, HIGH, MEDIUM, LOW)')
    .option('--json', 'Output in JSON format')
    .action(listVulnerabilities)

  scans
    .command('remove')
    .alias('rm')
    .description('Delete a vulnerability scan')
    .requiredOption('--id <id>', 'Scan ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(removeScan)

  scans
    .command('by-deployment')
    .description('Get the scan for a specific deployment')
    .requiredOption('--deployment-id <id>', 'Deployment ID')
    .option('--json', 'Output in JSON format')
    .action(scanByDeployment)
}

async function listScans(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  const page = options.page ? parseInt(options.page, 10) : undefined
  const pageSize = options.pageSize ? parseInt(options.pageSize, 10) : undefined

  const scansData = await withSpinner('Fetching vulnerability scans...', async () => {
    const { data, error } = await listProjectScans({
      client,
      path: { project_id: projectId },
      query: {
        ...(page && { page }),
        ...(pageSize && { page_size: pageSize }),
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(scansData)
    return
  }

  newline()
  header(`${icons.info} Vulnerability Scans for Project ${projectId} (${scansData.length})`)

  if (scansData.length === 0) {
    info('No vulnerability scans found')
    info(`Run: temps scans trigger --project-id ${projectId} --environment-id <env_id>`)
    newline()
    return
  }

  const columns: TableColumn<ScanResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Status', key: 'status', color: (v) => scanStatusColor(v) },
    { header: 'Scanner', key: 'scanner_type' },
    { header: 'Critical', accessor: (s) => String(s.critical_count), color: (v) => parseInt(v, 10) > 0 ? colors.error(v) : colors.muted(v) },
    { header: 'High', accessor: (s) => String(s.high_count), color: (v) => parseInt(v, 10) > 0 ? colors.error(v) : colors.muted(v) },
    { header: 'Medium', accessor: (s) => String(s.medium_count), color: (v) => parseInt(v, 10) > 0 ? colors.warning(v) : colors.muted(v) },
    { header: 'Low', accessor: (s) => String(s.low_count), color: (v) => colors.muted(v) },
    { header: 'Total', accessor: (s) => String(s.total_count) },
    { header: 'Started', accessor: (s) => formatRelativeTime(s.started_at), color: (v) => colors.muted(v) },
  ]

  printTable(scansData, columns, { style: 'minimal' })
  newline()
}

async function triggerScanAction(options: TriggerOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  const environmentId = parseInt(options.environmentId, 10)
  if (isNaN(environmentId)) {
    warning('Invalid environment ID')
    return
  }

  const result = await withSpinner('Triggering vulnerability scan...', async () => {
    const { data, error } = await triggerScan({
      client,
      path: { project_id: projectId },
      body: { environment_id: environmentId },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  success(`Scan triggered (ID: ${result?.scan_id})`)
  info(`Status: ${result?.status}`)
  info(`Check progress: temps scans show --id ${result?.scan_id}`)
}

async function latestScan(options: LatestOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  const environmentId = options.environmentId ? parseInt(options.environmentId, 10) : undefined
  if (options.environmentId && isNaN(environmentId!)) {
    warning('Invalid environment ID')
    return
  }

  const scan = await withSpinner('Fetching latest scan...', async () => {
    const { data, error } = await getLatestScan({
      client,
      path: { project_id: projectId },
      query: environmentId !== undefined ? { environment_id: environmentId } : undefined,
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? 'No scans found')
    }
    return data
  })

  if (options.json) {
    json(scan)
    return
  }

  displayScanDetails(scan)
}

async function environmentsScans(options: EnvironmentsOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  const scansData = await withSpinner('Fetching scans per environment...', async () => {
    const { data, error } = await getLatestScansPerEnvironment({
      client,
      path: { project_id: projectId },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(scansData)
    return
  }

  newline()
  header(`${icons.info} Latest Scans per Environment for Project ${projectId} (${scansData.length})`)

  if (scansData.length === 0) {
    info('No scans found for any environment')
    newline()
    return
  }

  const columns: TableColumn<ScanResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Env ID', accessor: (s) => s.environment_id != null ? String(s.environment_id) : '-' },
    { header: 'Status', key: 'status', color: (v) => scanStatusColor(v) },
    { header: 'Critical', accessor: (s) => String(s.critical_count), color: (v) => parseInt(v, 10) > 0 ? colors.error(v) : colors.muted(v) },
    { header: 'High', accessor: (s) => String(s.high_count), color: (v) => parseInt(v, 10) > 0 ? colors.error(v) : colors.muted(v) },
    { header: 'Medium', accessor: (s) => String(s.medium_count), color: (v) => parseInt(v, 10) > 0 ? colors.warning(v) : colors.muted(v) },
    { header: 'Low', accessor: (s) => String(s.low_count), color: (v) => colors.muted(v) },
    { header: 'Total', accessor: (s) => String(s.total_count) },
    { header: 'Started', accessor: (s) => formatRelativeTime(s.started_at), color: (v) => colors.muted(v) },
  ]

  printTable(scansData, columns, { style: 'minimal' })
  newline()
}

async function showScan(options: ShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid scan ID')
    return
  }

  const scan = await withSpinner('Fetching scan...', async () => {
    const { data, error } = await getScan({
      client,
      path: { scan_id: id },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Scan ${options.id} not found`)
    }
    return data
  })

  if (options.json) {
    json(scan)
    return
  }

  displayScanDetails(scan)
}

async function listVulnerabilities(options: VulnerabilitiesOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid scan ID')
    return
  }

  const vulns = await withSpinner('Fetching vulnerabilities...', async () => {
    const { data, error } = await getScanVulnerabilities({
      client,
      path: { scan_id: id },
      query: options.severity ? { severity: options.severity } : undefined,
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(vulns)
    return
  }

  newline()
  const titleSuffix = options.severity ? ` [${options.severity}]` : ''
  header(`${icons.info} Vulnerabilities for Scan #${id}${titleSuffix} (${vulns.length})`)

  if (vulns.length === 0) {
    info('No vulnerabilities found')
    newline()
    return
  }

  const columns: TableColumn<VulnerabilityResponse>[] = [
    { header: 'ID', key: 'vulnerability_id', width: 18 },
    { header: 'Severity', key: 'severity', color: (v) => severityColor(v) },
    { header: 'Package', key: 'package_name' },
    { header: 'Installed', key: 'installed_version' },
    { header: 'Fixed', accessor: (v) => v.fixed_version ?? '-', color: (v) => v !== '-' ? colors.success(v) : colors.muted(v) },
    { header: 'Title', accessor: (v) => truncate(v.title, 40) },
  ]

  printTable(vulns, columns, { style: 'minimal' })
  newline()
}

async function removeScan(options: RemoveOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid scan ID')
    return
  }

  // Get scan details first
  const { data: scan, error: getError } = await getScan({
    client,
    path: { scan_id: id },
  })

  if (getError || !scan) {
    warning(`Scan ${options.id} not found`)
    return
  }

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message: `Delete scan #${id} (${scan.status}, ${scan.total_count} vulnerabilities)?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Deleting scan...', async () => {
    const { error } = await deleteScan({
      client,
      path: { scan_id: id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Scan #${id} deleted`)
}

async function scanByDeployment(options: ByDeploymentOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const deploymentId = parseInt(options.deploymentId, 10)
  if (isNaN(deploymentId)) {
    warning('Invalid deployment ID')
    return
  }

  const scan = await withSpinner('Fetching scan for deployment...', async () => {
    const { data, error } = await getScanByDeployment({
      client,
      path: { deployment_id: deploymentId },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `No scan found for deployment ${options.deploymentId}`)
    }
    return data
  })

  if (options.json) {
    json(scan)
    return
  }

  displayScanDetails(scan)
}

// ============ Helpers ============

function displayScanDetails(scan: ScanResponse): void {
  newline()
  header(`${icons.info} Vulnerability Scan #${scan.id}`)
  keyValue('Status', scanStatusColor(scan.status))
  keyValue('Project ID', scan.project_id)
  if (scan.environment_id != null) {
    keyValue('Environment ID', scan.environment_id)
  }
  if (scan.deployment_id != null) {
    keyValue('Deployment ID', scan.deployment_id)
  }
  keyValue('Scanner', scan.scanner_type)
  if (scan.scanner_version) {
    keyValue('Scanner Version', scan.scanner_version)
  }
  if (scan.branch) {
    keyValue('Branch', scan.branch)
  }
  if (scan.commit_hash) {
    keyValue('Commit', scan.commit_hash)
  }
  newline()
  keyValue('Critical', scan.critical_count > 0 ? colors.error(String(scan.critical_count)) : String(scan.critical_count))
  keyValue('High', scan.high_count > 0 ? colors.error(String(scan.high_count)) : String(scan.high_count))
  keyValue('Medium', scan.medium_count > 0 ? colors.warning(String(scan.medium_count)) : String(scan.medium_count))
  keyValue('Low', String(scan.low_count))
  keyValue('Unknown', String(scan.unknown_count))
  keyValue('Total', colors.bold(String(scan.total_count)))
  newline()
  keyValue('Started', formatRelativeTime(scan.started_at))
  if (scan.completed_at) {
    keyValue('Completed', formatRelativeTime(scan.completed_at))
  }
  if (scan.error_message) {
    keyValue('Error', colors.error(scan.error_message))
  }
  keyValue('Created', formatRelativeTime(scan.created_at))
  keyValue('Updated', formatRelativeTime(scan.updated_at))
  newline()
}

export function scanStatusColor(status: string): string {
  switch (status.toLowerCase()) {
    case 'completed':
      return statusBadge('success')
    case 'running':
    case 'in_progress':
      return statusBadge('running')
    case 'pending':
    case 'queued':
      return statusBadge('pending')
    case 'failed':
    case 'error':
      return statusBadge('failed')
    default:
      return status
  }
}

export function severityColor(severity: string): string {
  switch (severity.toUpperCase()) {
    case 'CRITICAL':
      return colors.error(severity)
    case 'HIGH':
      return colors.error(severity)
    case 'MEDIUM':
      return colors.warning(severity)
    case 'LOW':
      return colors.muted(severity)
    default:
      return severity
  }
}

export function truncate(str: string, maxLength: number): string {
  if (str.length <= maxLength) return str
  return str.slice(0, maxLength - 3) + '...'
}
