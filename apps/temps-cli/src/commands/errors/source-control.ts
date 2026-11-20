// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// `temps errors issues`, `temps errors commits` and `temps errors ownership`:
// error groups and the project's repository (ADR 111, D5).

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  createErrorGroupIssue,
  getErrorGroupSuspectCommits,
  getErrorSourceControl,
  linkErrorGroupIssue,
  listErrorGroupIssues,
  listErrorOwnershipRules,
  refreshErrorGroupIssues,
  replaceErrorOwnershipRules,
  unlinkErrorGroupIssue,
} from '../../api/sdk.gen.js'
import type { IssueLinkView, OwnershipRuleRequest, SourceControlStatusView } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable } from '../../ui/table.js'
import { header, info, json, keyValue, success, warning } from '../../ui/output.js'
import { promptConfirm } from '../../ui/prompts.js'

/** Most ownership rules a project has. */
export const MAX_OWNERSHIP_RULES = 50

function wholeId(raw: string | undefined, flag: string): number {
  const id = Number(raw)
  if (raw === undefined || !Number.isInteger(id) || id <= 0) {
    throw new Error(`${flag} must be a positive whole number, got "${raw ?? ''}"`)
  }
  return id
}

/** "acme/shop#12", or the host of a plain link. */
export function issueLabel(link: Pick<IssueLinkView, 'provider' | 'repository' | 'number' | 'url'>): string {
  if (link.provider !== 'link' && link.repository && link.number) {
    return `${link.repository}#${link.number}`
  }
  try {
    return new URL(link.url).host
  } catch {
    return link.url
  }
}

/**
 * Ownership rules from a file: one `pattern user-id` per line, in the order
 * they are tried. Blank lines and `#` comments are skipped.
 */
export function parseOwnershipRules(text: string): OwnershipRuleRequest[] {
  const rules: OwnershipRuleRequest[] = []
  text.split(/\r?\n/).forEach((raw, index) => {
    const line = raw.replace(/#.*$/, '').trim()
    if (!line) return
    const [pattern, ownerText, ...rest] = line.split(/\s+/)
    if (!pattern || !ownerText || rest.length > 0) {
      throw new Error(`Line ${index + 1}: expected "<pattern> <user-id>", got "${raw.trim()}"`)
    }
    const owner = Number(ownerText)
    if (!Number.isInteger(owner) || owner <= 0) {
      throw new Error(`Line ${index + 1}: the owner must be a user ID, got "${ownerText}"`)
    }
    rules.push({ pattern, owner_user_id: owner })
  })
  if (rules.length > MAX_OWNERSHIP_RULES) {
    throw new Error(`A project has at most ${MAX_OWNERSHIP_RULES} ownership rules, the file has ${rules.length}`)
  }
  return rules
}

function printMissing(status: SourceControlStatusView): void {
  warning(status.reason ?? 'The project is not connected to a GitHub or GitLab repository')
  info(`Connect one in the console (${status.setup_path ?? '/git-providers'}) to open issues and read commits`)
}

function printLinks(links: IssueLinkView[]): void {
  if (links.length === 0) {
    info('No issue tracks this error yet. Open one with `temps errors issues open` or link one with `temps errors issues link`.')
    return
  }
  printTable(links, [
    { header: 'ID', accessor: (row) => String(row.id) },
    { header: 'Issue', accessor: (row) => issueLabel(row) },
    { header: 'State', accessor: (row) => row.state ?? '-' },
    { header: 'Title', accessor: (row) => row.title },
    { header: 'URL', accessor: (row) => row.url },
  ])
}

interface GroupOptions {
  projectId: string
  groupId: string
  json?: boolean
}

export function registerSourceControlCommands(errors: Command): void {
  const issues = errors.command('issues').description("Issues that track an error group, in the project's repository or any tracker")

  issues
    .command('list')
    .alias('ls')
    .description('List the issues linked to an error group')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--group-id <id>', 'Error group ID')
    .option('--refresh', 'Read again whether each GitHub or GitLab issue is open')
    .option('--json', 'Output in JSON format')
    .action(listAction)

  issues
    .command('open')
    .description("Open an issue for an error group in the project's GitHub or GitLab repository")
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--group-id <id>', 'Error group ID')
    .option('--title <title>', "Issue title (default: the group's title)")
    .option('--json', 'Output in JSON format')
    .action(openAction)

  issues
    .command('link')
    .description('Link an existing issue from any tracker to an error group')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--group-id <id>', 'Error group ID')
    .requiredOption('--url <url>', 'The issue URL')
    .option('--title <title>', "What to call it (default: the issue's title, or its URL)")
    .option('--json', 'Output in JSON format')
    .action(linkAction)

  issues
    .command('unlink')
    .description('Unlink an issue from an error group; the issue itself is left as it is')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--group-id <id>', 'Error group ID')
    .requiredOption('--link-id <id>', 'Issue link ID (from `temps errors issues list`)')
    .option('-y, --yes', 'Skip confirmation')
    .action(unlinkAction)

  errors
    .command('commits')
    .description('Show the commits that last touched the code an error group fails in')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--group-id <id>', 'Error group ID')
    .option('--json', 'Output in JSON format')
    .action(commitsAction)

  const ownership = errors.command('ownership').description('Rules that assign new error groups by the path of the failing code')

  ownership
    .command('list')
    .alias('ls')
    .description("List the project's ownership rules in the order they are tried")
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--json', 'Output in JSON format')
    .action(ownershipListAction)

  ownership
    .command('set')
    .description('Replace the ownership rules with those in a file: one "<pattern> <user-id>" per line')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--file <path>', 'Rules file; "-" reads standard input')
    .option('--clear', 'Remove every rule')
    .option('--json', 'Output in JSON format')
    .action(ownershipSetAction)
}

async function listAction(options: GroupOptions & { refresh?: boolean }): Promise<void> {
  const path = { project_id: wholeId(options.projectId, '--project-id'), group_id: wholeId(options.groupId, '--group-id') }
  await requireAuth()
  await setupClient()
  const result = await withSpinner(options.refresh ? 'Reading the issues...' : 'Loading issues...', async () => {
    const { data, error } = options.refresh
      ? await refreshErrorGroupIssues({ client, path })
      : await listErrorGroupIssues({ client, path })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(result.data)
    return
  }
  header(`Issues of error group #${path.group_id}`)
  printLinks(result.data)
  if (result.failed_refreshes > 0) {
    warning(`${result.failed_refreshes} issue(s) could not be read; they keep their last state`)
  }
}

async function openAction(options: GroupOptions & { title?: string }): Promise<void> {
  const path = { project_id: wholeId(options.projectId, '--project-id'), group_id: wholeId(options.groupId, '--group-id') }
  await requireAuth()
  await setupClient()
  const status = await withSpinner('Checking the repository...', async () => {
    const { data, error } = await getErrorSourceControl({ client, path: { project_id: path.project_id } })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (!status.configured) {
    printMissing(status)
    process.exitCode = 1
    return
  }
  const link = await withSpinner(`Opening an issue in ${status.repository}...`, async () => {
    const { data, error } = await createErrorGroupIssue({ client, path, body: { title: options.title ?? null } })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(link)
    return
  }
  success(`Opened ${issueLabel(link)}: ${link.url}`)
}

async function linkAction(options: GroupOptions & { url: string; title?: string }): Promise<void> {
  const path = { project_id: wholeId(options.projectId, '--project-id'), group_id: wholeId(options.groupId, '--group-id') }
  await requireAuth()
  await setupClient()
  const link = await withSpinner('Linking the issue...', async () => {
    const { data, error } = await linkErrorGroupIssue({
      client,
      path,
      body: { url: options.url, title: options.title ?? null },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(link)
    return
  }
  success(`Linked ${issueLabel(link)}${link.state ? ` (${link.state})` : ''}`)
}

async function unlinkAction(options: GroupOptions & { linkId: string; yes?: boolean }): Promise<void> {
  const path = {
    project_id: wholeId(options.projectId, '--project-id'),
    group_id: wholeId(options.groupId, '--group-id'),
    link_id: wholeId(options.linkId, '--link-id'),
  }
  await requireAuth()
  await setupClient()
  if (!options.yes) {
    const confirmed = await promptConfirm({
      message: `Unlink issue link #${path.link_id}? The issue stays as it is in its tracker.`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }
  await withSpinner(`Unlinking #${path.link_id}...`, async () => {
    const { error } = await unlinkErrorGroupIssue({ client, path })
    if (error) throw new Error(getErrorMessage(error))
  })
  success(`Unlinked #${path.link_id}`)
}

async function commitsAction(options: GroupOptions): Promise<void> {
  const projectId = wholeId(options.projectId, '--project-id')
  const groupId = wholeId(options.groupId, '--group-id')
  await requireAuth()
  await setupClient()
  const status = await withSpinner('Checking the repository...', async () => {
    const { data, error } = await getErrorSourceControl({ client, path: { project_id: projectId } })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (!status.configured) {
    if (options.json) {
      json(status)
    } else {
      printMissing(status)
    }
    return
  }
  const result = await withSpinner('Reading commits...', async () => {
    const { data, error } = await getErrorGroupSuspectCommits({
      client,
      path: { project_id: projectId, group_id: groupId },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  header(`Suspect commits of error group #${groupId}`)
  if (result.path) {
    keyValue('Fails in', `${result.path}${result.lineno ? `:${result.lineno}` : ''}${result.function ? ` in ${result.function}` : ''}`)
    keyValue('Read at', result.at ? result.at.slice(0, 7) : 'the default branch')
  }
  if (result.commits.length === 0) {
    info(result.reason ?? 'No commit touched the file')
    return
  }
  printTable(result.commits, [
    { header: 'Commit', accessor: (row) => row.sha.slice(0, 7) },
    { header: 'Summary', accessor: (row) => row.summary },
    { header: 'Author', accessor: (row) => row.author_name ?? '-' },
    { header: 'Date', accessor: (row) => (row.authored_at ? new Date(row.authored_at).toLocaleDateString() : '-') },
  ])
}

async function ownershipListAction(options: { projectId: string; json?: boolean }): Promise<void> {
  const projectId = wholeId(options.projectId, '--project-id')
  await requireAuth()
  await setupClient()
  const rules = await withSpinner('Loading ownership rules...', async () => {
    const { data, error } = await listErrorOwnershipRules({ client, path: { project_id: projectId } })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data.data
  })
  if (options.json) {
    json(rules)
    return
  }
  header('Ownership rules')
  if (rules.length === 0) {
    info('No rules. Set some with: temps errors ownership set --project-id <id> --file rules.txt')
    info('Each line is "<pattern> <user-id>", for example: src/billing/ 4')
    return
  }
  printTable(rules, [
    { header: '#', accessor: (row) => String(row.position + 1) },
    { header: 'Pattern', accessor: (row) => row.pattern },
    { header: 'Owner', accessor: (row) => `${row.owner_name ?? 'user'} (#${row.owner_user_id})` },
  ])
}

async function ownershipSetAction(options: {
  projectId: string
  file?: string
  clear?: boolean
  json?: boolean
}): Promise<void> {
  const projectId = wholeId(options.projectId, '--project-id')
  if (!!options.file === !!options.clear) {
    throw new Error('Give either --file with the rules or --clear')
  }
  let rules: OwnershipRuleRequest[] = []
  if (options.file) {
    const source = options.file === '-' ? Bun.stdin : Bun.file(options.file)
    if (options.file !== '-' && !(await source.exists())) {
      throw new Error(`No rules file at ${options.file}`)
    }
    const text = await source.text()
    rules = parseOwnershipRules(text)
  }
  await requireAuth()
  await setupClient()
  const saved = await withSpinner('Saving ownership rules...', async () => {
    const { data, error } = await replaceErrorOwnershipRules({
      client,
      path: { project_id: projectId },
      body: { rules },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data.data
  })
  if (options.json) {
    json(saved)
    return
  }
  success(saved.length === 0 ? 'Removed every ownership rule' : `Saved ${saved.length} ownership rule(s)`)
}
