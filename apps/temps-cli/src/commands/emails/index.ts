import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listEmails,
  sendEmail,
  getEmail,
  getEmailStats,
  validateEmail,
} from '../../api/sdk.gen.js'
import type { EmailResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptText, promptConfirm } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning, keyValue, formatDate } from '../../ui/output.js'

interface ListOptions {
  json?: boolean
  page?: string
  pageSize?: string
  status?: string
  domainId?: string
  projectId?: string
  fromAddress?: string
}

interface SendOptions {
  to?: string
  subject?: string
  body?: string
  from?: string
  yes?: boolean
}

interface ShowOptions {
  id: string
  json?: boolean
}

interface StatsOptions {
  json?: boolean
}

interface ValidateOptions {
  email?: string
  json?: boolean
}

export function registerEmailsCommands(program: Command): void {
  const emails = program
    .command('emails')
    .alias('email')
    .description('Manage and send emails')

  emails
    .command('list')
    .alias('ls')
    .description('List sent emails')
    .option('--json', 'Output in JSON format')
    .option('--page <n>', 'Page number')
    .option('--page-size <n>', 'Items per page')
    .option('--status <status>', 'Filter by status (sent, delivered, failed)')
    .option('--domain-id <id>', 'Filter by domain ID')
    .option('--project-id <id>', 'Filter by project ID')
    .option('--from-address <email>', 'Filter by sender address')
    .action(listEmailsAction)

  emails
    .command('send')
    .description('Send an email')
    .option('--to <email>', 'Recipient email address')
    .option('--subject <subject>', 'Email subject')
    .option('--body <body>', 'Email body')
    .option('--from <email>', 'Sender email address')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(sendEmailAction)

  emails
    .command('show')
    .description('Show email details')
    .requiredOption('--id <id>', 'Email ID')
    .option('--json', 'Output in JSON format')
    .action(showEmail)

  emails
    .command('stats')
    .description('Get email statistics')
    .option('--json', 'Output in JSON format')
    .action(emailStats)

  emails
    .command('validate')
    .description('Validate an email address')
    .option('--email <email>', 'Email address to validate')
    .option('--json', 'Output in JSON format')
    .action(validateEmailAction)
}

async function listEmailsAction(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const page = options.page ? parseInt(options.page, 10) : undefined
  const pageSize = options.pageSize ? parseInt(options.pageSize, 10) : undefined
  const domainId = options.domainId ? parseInt(options.domainId, 10) : undefined
  const projectId = options.projectId ? parseInt(options.projectId, 10) : undefined

  const response = await withSpinner('Fetching emails...', async () => {
    const { data, error } = await listEmails({
      client,
      query: {
        ...(page && { page }),
        ...(pageSize && { page_size: pageSize }),
        ...(options.status && { status: options.status }),
        ...(domainId && { domain_id: domainId }),
        ...(projectId && { project_id: projectId }),
        ...(options.fromAddress && { from_address: options.fromAddress }),
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  const emailsData = response?.data ?? []

  if (options.json) {
    json(response)
    return
  }

  newline()
  header(`${icons.info} Sent Emails (${response?.total ?? emailsData.length})`)

  if (emailsData.length === 0) {
    info('No emails sent yet')
    info('Run: temps emails send --to user@example.com --subject "Hello" --body "World" -y')
    newline()
    return
  }

  const columns: TableColumn<EmailResponse>[] = [
    { header: 'ID', key: 'id', width: 8 },
    { header: 'To', accessor: (e) => e.to_addresses.join(', '), color: (v) => colors.bold(v) },
    { header: 'Subject', key: 'subject', color: (v) => v.length > 40 ? v.slice(0, 40) + '...' : v },
    { header: 'Status', key: 'status', color: (v) => statusBadge(v === 'delivered' || v === 'sent' ? 'active' : v === 'failed' ? 'error' : 'pending') },
    { header: 'Sent', accessor: (e) => e.sent_at ? new Date(e.sent_at).toLocaleDateString() : new Date(e.created_at).toLocaleDateString() },
  ]

  printTable(emailsData, columns, { style: 'minimal' })
  newline()
}

async function sendEmailAction(options: SendOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  let to: string
  let subject: string
  let body: string
  let from: string | undefined

  const isAutomation = options.yes && options.to && options.subject && options.body

  if (isAutomation) {
    to = options.to!
    subject = options.subject!
    body = options.body!
    from = options.from
  } else {
    to = options.to || await promptText({
      message: 'Recipient email address',
      required: true,
    })

    subject = options.subject || await promptText({
      message: 'Email subject',
      required: true,
    })

    body = options.body || await promptText({
      message: 'Email body',
      required: true,
    })

    from = options.from || await promptText({
      message: 'Sender email address (optional)',
      default: '',
    }) || undefined

    if (!options.yes) {
      newline()
      info(`To: ${to}`)
      info(`Subject: ${subject}`)
      if (from) info(`From: ${from}`)
      newline()

      const confirmed = await promptConfirm({
        message: 'Send this email?',
        default: true,
      })
      if (!confirmed) {
        info('Cancelled')
        return
      }
    }
  }

  await withSpinner('Sending email...', async () => {
    const { error } = await sendEmail({
      client,
      body: {
        to: [to],
        subject,
        text: body,
        from: from ?? to,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('Email sent successfully')
  info(`Recipient: ${to}`)
}

async function showEmail(options: ShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = options.id

  const email = await withSpinner('Fetching email...', async () => {
    const { data, error } = await getEmail({
      client,
      path: { id },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Email ${options.id} not found`)
    }
    return data
  })

  if (options.json) {
    json(email)
    return
  }

  newline()
  header(`${icons.info} Email #${email.id}`)
  keyValue('From', email.from_name ? `${email.from_name} <${email.from_address}>` : email.from_address)
  keyValue('To', email.to_addresses.join(', '))
  if (email.cc_addresses && email.cc_addresses.length > 0) {
    keyValue('CC', email.cc_addresses.join(', '))
  }
  if (email.bcc_addresses && email.bcc_addresses.length > 0) {
    keyValue('BCC', email.bcc_addresses.join(', '))
  }
  keyValue('Subject', email.subject)
  keyValue('Status', statusBadge(
    email.status === 'delivered' || email.status === 'sent' ? 'active' :
    email.status === 'failed' ? 'error' : 'pending'
  ))
  keyValue('Created', formatDate(email.created_at))
  if (email.sent_at) {
    keyValue('Sent', formatDate(email.sent_at))
  }
  if (email.domain_id !== null && email.domain_id !== undefined) {
    keyValue('Domain ID', email.domain_id)
  }
  if (email.project_id !== null && email.project_id !== undefined) {
    keyValue('Project ID', email.project_id)
  }
  if (email.tags && email.tags.length > 0) {
    keyValue('Tags', email.tags.join(', '))
  }

  newline()
  header('Tracking')
  keyValue('Open Tracking', email.track_opens ? colors.success('Enabled') : 'Disabled')
  keyValue('Opens', email.open_count)
  if (email.first_opened_at) {
    keyValue('First Opened', formatDate(email.first_opened_at))
  }
  keyValue('Click Tracking', email.track_clicks ? colors.success('Enabled') : 'Disabled')
  keyValue('Clicks', email.click_count)
  if (email.first_clicked_at) {
    keyValue('First Clicked', formatDate(email.first_clicked_at))
  }

  const body = email.text_body || email.html_body
  if (body) {
    newline()
    header('Body')
    // Email bodies are attacker-controlled (anything sent through the
    // platform's send API) — strip ANSI/control escape sequences before
    // printing so a crafted body can't manipulate the cursor, hide/spoof
    // output, or set the terminal window title.
    console.log(`  ${body.replace(/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/g, '')}`)
  }

  if (email.error_message) {
    newline()
    warning(`Error: ${email.error_message}`)
  }

  newline()
}

async function emailStats(options: StatsOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const stats = await withSpinner('Fetching email statistics...', async () => {
    const { data, error } = await getEmailStats({ client })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? 'Failed to fetch email statistics')
    }
    return data
  })

  if (options.json) {
    json(stats)
    return
  }

  newline()
  header(`${icons.info} Email Statistics`)
  keyValue('Total', stats.total)
  keyValue('Sent', colors.success(String(stats.sent)))
  keyValue('Queued', colors.warning(String(stats.queued)))
  keyValue('Failed', colors.error(String(stats.failed)))
  keyValue('Captured (no provider configured)', stats.captured)
  newline()
}

async function validateEmailAction(options: ValidateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  let email: string

  if (options.email) {
    email = options.email
  } else {
    email = await promptText({
      message: 'Email address to validate',
      required: true,
    })
  }

  const result = await withSpinner(`Validating ${email}...`, async () => {
    const { data, error } = await validateEmail({
      client,
      body: { email },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? 'Failed to validate email')
    }
    return data
  })

  if (options.json) {
    json(result)
    return
  }

  const resultData = result as Record<string, unknown>

  newline()
  header(`${icons.info} Email Validation: ${email}`)
  keyValue('Email', email)
  if (resultData.is_valid !== undefined) {
    keyValue('Valid', resultData.is_valid ? colors.success('Yes') : colors.error('No'))
  }
  if (resultData.format_valid !== undefined) {
    keyValue('Format Valid', resultData.format_valid ? colors.success('Yes') : colors.error('No'))
  }
  if (resultData.mx_valid !== undefined) {
    keyValue('MX Records Valid', resultData.mx_valid ? colors.success('Yes') : colors.error('No'))
  }
  if (resultData.disposable !== undefined) {
    keyValue('Disposable', resultData.disposable ? colors.warning('Yes') : 'No')
  }
  if (resultData.reason) {
    keyValue('Reason', resultData.reason as string)
  }
  if (resultData.suggestion) {
    info(`Did you mean: ${colors.bold(resultData.suggestion as string)}?`)
  }
  newline()
}
