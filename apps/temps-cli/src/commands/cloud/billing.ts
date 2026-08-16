import type { Command } from 'commander'
import { execSync } from 'node:child_process'
import { cloudFetch } from '../../lib/cloud-client.js'
import { withSpinner } from '../../ui/spinner.js'
import {
  newline,
  header,
  keyValue,
  icons,
  json as jsonOutput,
  colors,
  box,
  info,
} from '../../ui/output.js'

// --- Types ---

interface BillingOverview {
  plan: string
  status: string
  currentPeriodStart?: string
  currentPeriodEnd?: string
  monthlyPriceCents?: number
  hasPaymentMethod?: boolean
}

interface BillingUsage {
  vpsCount?: number
  vpsLimit?: number
  serversCount?: number
  serversLimit?: number
  tunnelsCount?: number
  tunnelsLimit?: number
}

interface CheckoutResponse {
  url: string
}

// --- Helpers ---

function openBrowser(url: string): void {
  try {
    if (process.platform === 'darwin') {
      execSync(`open "${url}"`)
    } else if (process.platform === 'win32') {
      execSync(`start "" "${url}"`)
    } else {
      execSync(`xdg-open "${url}"`)
    }
  } catch {
    // Browser open failed, user will use the URL directly
  }
}

export function formatPrice(cents: number): string {
  if (cents === 0) return colors.success('free')
  return `€${(cents / 100).toFixed(2)}/mo`
}

export function usageBar(used: number, limit: number): string {
  const pct = limit > 0 ? Math.min(used / limit, 1) : 0
  const width = 20
  const filled = Math.round(pct * width)
  const empty = width - filled
  const bar = '█'.repeat(filled) + '░'.repeat(empty)
  const colorFn = pct >= 0.9 ? colors.error : pct >= 0.7 ? colors.warning : colors.success
  return `${colorFn(bar)} ${used}/${limit}`
}

// --- Commands ---

async function billingOverview(options: { json?: boolean }): Promise<void> {
  const data = await withSpinner('Fetching billing info...', () =>
    cloudFetch<BillingOverview>('/api/billing/overview')
  )

  if (options.json) {
    jsonOutput(data)
    return
  }

  newline()
  header(`${icons.star} Billing Overview`)
  keyValue('Plan', colors.bold(data.plan))
  keyValue('Status', data.status)
  if (data.monthlyPriceCents !== undefined) {
    keyValue('Price', formatPrice(data.monthlyPriceCents))
  }
  if (data.currentPeriodStart) {
    keyValue('Period Start', data.currentPeriodStart)
  }
  if (data.currentPeriodEnd) {
    keyValue('Period End', data.currentPeriodEnd)
  }
  if (data.hasPaymentMethod !== undefined) {
    keyValue('Payment Method', data.hasPaymentMethod ? 'on file' : 'none')
  }
  newline()
}

async function billingUsage(options: { json?: boolean }): Promise<void> {
  const data = await withSpinner('Fetching usage...', () =>
    cloudFetch<BillingUsage>('/api/billing/usage')
  )

  if (options.json) {
    jsonOutput(data)
    return
  }

  newline()
  header('Usage')
  if (data.vpsLimit !== undefined) {
    keyValue('VPS', usageBar(data.vpsCount ?? 0, data.vpsLimit))
  }
  if (data.serversLimit !== undefined) {
    keyValue('Servers', usageBar(data.serversCount ?? 0, data.serversLimit))
  }
  if (data.tunnelsLimit !== undefined) {
    keyValue('Tunnels', usageBar(data.tunnelsCount ?? 0, data.tunnelsLimit))
  }
  newline()
}

async function billingUpgrade(options: { noBrowser?: boolean; yearly?: boolean }): Promise<void> {
  const billingCycle = options.yearly ? 'yearly' : 'monthly'
  const data = await withSpinner('Creating checkout session...', () =>
    cloudFetch<CheckoutResponse>('/api/billing/checkout', {
      method: 'POST',
      body: JSON.stringify({ plan: 'app', billingCycle }),
    })
  )

  newline()
  box(
    `Complete your upgrade at:\n\n${colors.primary(data.url)}`,
    `${icons.star} Upgrade to Pro`
  )
  newline()

  if (!options.noBrowser) {
    info('Opening browser...')
    openBrowser(data.url)
  }
}

// --- Registration ---

export function registerCloudBillingCommands(cloud: Command): void {
  const billing = cloud
    .command('billing')
    .description('Manage Temps Cloud billing and subscription')

  billing
    .command('overview')
    .description('Show billing overview')
    .option('--json', 'Output as JSON')
    .action(billingOverview)

  billing
    .command('usage')
    .description('Show usage and limits')
    .option('--json', 'Output as JSON')
    .action(billingUsage)

  billing
    .command('upgrade')
    .description('Upgrade your plan')
    .option('--yearly', 'Use yearly billing cycle (default: monthly)')
    .option('--no-browser', 'Don\'t open browser, just show the URL')
    .action(billingUpgrade)
}
