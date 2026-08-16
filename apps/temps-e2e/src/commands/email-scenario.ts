/**
 * Email-via-Mailpit lifecycle against a live Temps instance:
 *   1. create an SMTP email provider pointed at a local Mailpit instance
 *   2. register + verify a throwaway sending domain against it (SMTP domains
 *      verify immediately -- see `createVerifiedEmailDomain` in flows.ts)
 *   3. send a real email with open + click tracking enabled
 *   4. confirm ACTUAL receipt via Mailpit's own REST API (not just that the
 *      send call returned success -- `send_email` silently falls back to a
 *      "captured, never sent" status on any provider/domain problem and
 *      still returns 201, so a Mailpit-side check is the only real proof)
 *   5. simulate a recipient opening the email + clicking its one link, and
 *      confirm both landed as real `email_events` rows
 *   6. tear everything down (provider, domain) unless --keep
 *
 * Requires a target instance whose outbound SMTP can actually reach Mailpit
 * (both processes on the same host is the common case) — see
 * apps/temps-e2e/README.md.
 */
import { makeClient, resolveConfig } from '../lib/client.ts'
import {
  createSmtpProvider,
  createVerifiedEmailDomain,
  sendTrackedEmail,
  hitOpenTrackingPixel,
  hitClickTrackingLink,
  waitForMailpitMessage,
  waitForEmailEvent,
  teardownEmailResources,
  makeRunId,
} from '../lib/flows.ts'

export interface EmailScenarioOptions {
  smtpHost?: string
  smtpPort?: string
  mailpitUrl?: string
  keep?: boolean
  json?: boolean
  connection: { url?: string; apiKey?: string }
}

interface StepLog {
  step: string
  ok: boolean
  detail?: string
  ms?: number
}

interface EmailScenarioResult {
  runId: string
  ok: boolean
  emailId?: string
  steps: StepLog[]
}

export async function emailScenarioCommand(opts: EmailScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps email-via-Mailpit scenario  ->  ${cfg.url}`)

  const runId = makeRunId(Date.now())
  const steps: StepLog[] = []
  const step = async <T>(name: string, fn: () => Promise<T>): Promise<T> => {
    const t0 = performance.now()
    log(`\n▶ ${name}`)
    try {
      const r = await fn()
      const ms = performance.now() - t0
      steps.push({ step: name, ok: true, ms })
      log(`  ✓ ${name} (${(ms / 1000).toFixed(1)}s)`)
      return r
    } catch (e) {
      const ms = performance.now() - t0
      steps.push({ step: name, ok: false, detail: (e as Error).message, ms })
      log(`  ✗ ${name}: ${(e as Error).message}`)
      throw e
    }
  }

  const smtpHost = opts.smtpHost ?? 'localhost'
  const smtpPort = Number(opts.smtpPort ?? '1025')
  const mailpitUrl = opts.mailpitUrl ?? 'http://localhost:8025'
  const domain = `${runId}.e2e.test`
  const from = `noreply@${domain}`
  const to = `recipient@${domain}`
  const subject = `e2e email test ${runId}`

  let providerId: number | undefined
  let domainId: number | undefined
  let emailId: string | undefined

  try {
    const provider = await step('create SMTP provider (pointed at Mailpit)', () =>
      createSmtpProvider(client, { name: `${runId}-mailpit`, host: smtpHost, port: smtpPort }),
    )
    providerId = provider.id
    log(`  provider #${provider.id}`)

    const emailDomain = await step('create + verify sending domain', () =>
      createVerifiedEmailDomain(client, { domain, providerId: provider.id }),
    )
    domainId = emailDomain.id
    log(`  domain #${emailDomain.id} (${emailDomain.domain})`)

    const sent = await step('send email with open + click tracking enabled', () =>
      sendTrackedEmail(client, { from, to, subject, linkUrl: 'https://example.com/e2e-click-target' }),
    )
    emailId = sent.emailId
    log(`  email #${sent.emailId}  tracked link #${sent.linkIndex}`)

    await step('confirm actual receipt in Mailpit (not just a 201 from the send call)', () =>
      waitForMailpitMessage(mailpitUrl, subject),
    )

    await step('simulate recipient opening the email', async () => {
      const status = await hitOpenTrackingPixel(cfg.url, sent.emailId)
      if (status !== 200) throw new Error(`open-tracking pixel returned HTTP ${status}`)
    })
    await step('confirm open event recorded', () => waitForEmailEvent(client, sent.emailId, 'opened'))

    await step('simulate recipient clicking the tracked link', async () => {
      const status = await hitClickTrackingLink(cfg.url, sent.emailId, sent.linkIndex)
      if (status < 300 || status >= 400) {
        throw new Error(`click-tracking link returned HTTP ${status} (expected a redirect)`)
      }
    })
    await step('confirm click event recorded', () => waitForEmailEvent(client, sent.emailId, 'clicked'))
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (opts.keep) {
      log(`\n(kept resources: provider=${providerId} domain=${domainId} email=${emailId})`)
    } else {
      const errors = await teardownEmailResources(client, { providerId, domainId })
      log(`\n▶ teardown: removed email provider + domain${errors.length ? ` (${errors.length} errors)` : ''}`)
      for (const e of errors) log(`    ! ${e}`)
    }
  }

  const ok = steps.length > 0 && steps.every((s) => s.ok)
  const result: EmailScenarioResult = { runId, ok, emailId, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ email-scenario PASSED' : '❌ email-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
