/**
 * Vercel migration — the one client-side migration path left in this CLI.
 *
 * `temps migrate` (registered in `../imports/index.ts`) is the single
 * user-facing migration command. Every other source (Coolify, Dokploy,
 * CapRover, Portainer, Kamal, Kubernetes, Docker) goes through the real
 * backend importer pipeline (`temps-import`) via that command. Vercel has
 * no backend importer yet, so `temps migrate` delegates to
 * `runVercelMigrationWizard` below for that one source instead of
 * reimplementing a second client-side adapter system for platforms that
 * already have a proper backend implementation.
 */

import { requireAuth } from '../../config/store.js'
import { setupClient } from '../../lib/api-client.js'
import {
  header,
  newline,
  icons,
  success,
  error as errorOutput,
  warning,
  info,
  box,
} from '../../ui/output.js'
import {
  promptSelect,
  promptText,
  promptPassword,
  promptConfirm,
  promptCheckbox,
  promptSearch,
} from '../../ui/prompts.js'
import { withSpinner } from '../../ui/spinner.js'

import type { PlatformId, PlatformCredentials, MigrationPlan } from './types.js'
import { PLATFORMS } from './types.js'
import type { PlatformAdapter } from './adapters/base.js'
import { VercelAdapter } from './adapters/vercel.js'
import { displayMigrationPlan, displayMigrationResult } from './plan-display.js'
import { runPreChecks, runPostChecks } from './verification.js'
import { executeMigration } from './orchestrator.js'

// ---------------------------------------------------------------------------
// Adapter registry (Vercel-only — see the file header)
// ---------------------------------------------------------------------------

function getAdapter(platformId: PlatformId): PlatformAdapter {
  switch (platformId) {
    case 'vercel':
      return new VercelAdapter()
    default:
      throw new Error(`Unknown platform: ${platformId}`)
  }
}

/**
 * Entry point called by `temps migrate` (in `../imports/index.ts`) when the
 * user picks Vercel as the source. Runs the full interactive wizard:
 * credentials → discover → pick project → plan → review/modify → confirm →
 * execute → pre/post verification.
 */
export async function runVercelMigrationWizard(): Promise<void> {
  await runMigrationWizard({ from: 'vercel' })
}

// ---------------------------------------------------------------------------
// Interactive credential collection
// ---------------------------------------------------------------------------

async function collectCredentials(platformId: PlatformId): Promise<PlatformCredentials> {
  const platformInfo = PLATFORMS[platformId]

  newline()
  header(`${icons.key} ${platformInfo.name} Credentials`)

  // Show step-by-step instructions for getting the API token
  const instructions = platformInfo.tokenInstructions
    .map((step, i) => `${i + 1}. ${step}`)
    .join('\n')
  box(instructions, `How to get your ${platformInfo.name} API token`)
  newline()

  const token = await promptPassword({
    message: `${platformInfo.name} API token`,
    validate: (value) => (value.length > 0 ? true : 'Token is required'),
  })

  let teamId: string | undefined
  let baseUrl: string | undefined

  if (platformId === 'vercel') {
    const hasTeam = await promptConfirm({
      message: 'Are you migrating from a Vercel team/organization?',
      default: false,
    })
    if (hasTeam) {
      teamId = await promptText({
        message: 'Team ID or slug',
        required: true,
      })
    }
  }

  return { token, teamId, baseUrl }
}

// ---------------------------------------------------------------------------
// Select platform
// ---------------------------------------------------------------------------

async function selectPlatform(fromOption?: string): Promise<PlatformId> {
  if (fromOption) {
    const normalized = fromOption.toLowerCase() as PlatformId
    if (!PLATFORMS[normalized]) {
      throw new Error(`Unknown platform: ${fromOption}. Available: ${Object.keys(PLATFORMS).join(', ')}`)
    }
    return normalized
  }

  return promptSelect<PlatformId>({
    message: 'Select source platform',
    choices: Object.values(PLATFORMS).map((p) => ({
      name: `${p.name} — ${p.description}`,
      value: p.id,
    })),
  })
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

async function runMigrationWizard(options: { from?: string }): Promise<void> {
  await requireAuth()
  await setupClient()

  // Step 1: Select platform
  const platformId = await selectPlatform(options.from)
  const adapter = getAdapter(platformId)

  // Step 2: Collect and validate credentials
  const creds = await collectCredentials(platformId)

  const validation = await withSpinner(
    `Validating ${PLATFORMS[platformId].name} credentials...`,
    () => adapter.validateCredentials(creds),
    {
      successText: (v) => `Authenticated: ${v.message}`,
      failText: () => 'Authentication failed',
    }
  )

  if (!validation.valid) {
    errorOutput(validation.message)
    return
  }

  // Step 3: Discover projects
  const projects = await withSpinner(
    `Discovering projects on ${PLATFORMS[platformId].name}...`,
    () => adapter.discoverProjects(creds),
    {
      successText: (p) => `Found ${p.length} project(s)`,
    }
  )

  if (projects.length === 0) {
    warning('No projects found on the source platform')
    return
  }

  // Step 4: Select project(s) to migrate
  const selectedProjectId = await promptSearch<string>({
    message: 'Select project to migrate',
    choices: projects.map((p) => ({
      name: p.name,
      value: p.id,
      description: [
        p.framework ? `[${p.framework}]` : null,
        p.gitUrl ? p.gitUrl : null,
        p.updatedAt ? `updated: ${new Date(p.updatedAt).toLocaleDateString()}` : null,
      ]
        .filter(Boolean)
        .join(' | '),
    })),
  })

  // Step 5: Snapshot the project
  const snapshot = await withSpinner(
    'Taking project snapshot...',
    () => adapter.snapshotProject(creds, selectedProjectId),
    {
      successText: (s) =>
        `Snapshot: ${s.envVars.length} env vars, ${s.services.length} services, ${s.domains.length} domains`,
    }
  )

  // Step 6: Generate migration plan
  const plan = adapter.generatePlan(snapshot)

  // Step 7: Display the plan
  displayMigrationPlan(plan)

  // Step 8: Allow user to modify the plan
  const modifiedPlan = await allowPlanModification(plan)

  // Step 9: Run pre-migration checks
  newline()
  header(`${icons.info} Pre-Migration Verification`)

  const preResults = await withSpinner('Running pre-migration checks...', () =>
    runPreChecks(modifiedPlan)
  )

  let hasBlockingErrors = false
  for (const check of preResults) {
    const icon = check.passed ? icons.success : check.severity === 'error' ? icons.error : icons.warning
    console.log(`  ${icon} ${check.name}: ${check.message}`)
    if (!check.passed && check.severity === 'error') {
      hasBlockingErrors = true
    }
  }

  if (hasBlockingErrors) {
    newline()
    errorOutput('Pre-migration checks failed. Fix the issues above before proceeding.')
    return
  }

  // Step 10: Final confirmation
  newline()
  const confirmed = await promptConfirm({
    message: `Execute migration? This will create the project "${modifiedPlan.project.name}" in Temps.`,
    default: false,
  })

  if (!confirmed) {
    info('Migration cancelled')
    return
  }

  // Step 11: Execute
  newline()
  const result = await withSpinner('Executing migration...', () => executeMigration(modifiedPlan), {
    successText: () => 'Migration steps completed',
    failText: () => 'Migration encountered errors',
  })

  // Step 12: Display results
  displayMigrationResult(result)

  // Step 13: Post-migration verification
  header(`${icons.info} Post-Migration Verification`)

  const postResults = await withSpinner('Running post-migration checks...', () =>
    runPostChecks(modifiedPlan, result)
  )

  for (const check of postResults) {
    const icon = check.passed ? icons.success : check.severity === 'error' ? icons.error : icons.warning
    console.log(`  ${icon} ${check.name}: ${check.message}`)
  }

  // Step 14: Final summary
  newline()
  if (result.success) {
    success('Migration completed successfully!')
    if (result.projectSlug) {
      info(`View your project: temps projects show ${result.projectSlug}`)
    }
  } else {
    warning('Migration completed with errors. Review the results above.')
  }

  // Show manual actions reminder
  if (modifiedPlan.summary.manualActions.length > 0) {
    newline()
    header(`${icons.clock} Don't forget these manual actions:`)
    for (const action of modifiedPlan.summary.manualActions) {
      console.log(`  ${icons.arrow} [${action.timing.toUpperCase()}] ${action.description}`)
    }
  }

  newline()
}

// ---------------------------------------------------------------------------
// Plan modification (interactive)
// ---------------------------------------------------------------------------

async function allowPlanModification(plan: MigrationPlan): Promise<MigrationPlan> {
  const modify = await promptConfirm({
    message: 'Would you like to modify any part of the plan?',
    default: false,
  })

  if (!modify) return plan

  // Offer modification options
  const what = await promptCheckbox<string>({
    message: 'What would you like to modify?',
    choices: [
      { name: 'Project name', value: 'name' },
      { name: 'Skip/include specific env vars', value: 'envvars' },
      { name: 'Skip/include specific services', value: 'services' },
      { name: 'Skip/include specific domains', value: 'domains' },
      { name: 'Skip/include specific steps', value: 'steps' },
    ],
  })

  // Clone plan for modification
  const modified = structuredClone(plan)

  if (what.includes('name')) {
    modified.project.name = await promptText({
      message: 'New project name',
      default: modified.project.name,
      required: true,
    })
    // Update the create-project step title
    const projectStep = modified.steps.find((s) => s.id === 'create-project')
    if (projectStep) {
      projectStep.title = `Create project "${modified.project.name}"`
    }
  }

  if (what.includes('envvars') && modified.envVars.length > 0) {
    const toSkip = await promptCheckbox<number>({
      message: 'Select env vars to SKIP (unselected will be included)',
      choices: modified.envVars.map((ev, i) => ({
        name: `${ev.key}${ev.isSecret ? ' (secret)' : ''} ${ev.skip ? '[auto-managed]' : ''}`,
        value: i,
      })),
    })
    for (let i = 0; i < modified.envVars.length; i++) {
      modified.envVars[i]!.skip = toSkip.includes(i)
    }
  }

  if (what.includes('services') && modified.services.length > 0) {
    const toSkip = await promptCheckbox<number>({
      message: 'Select services to SKIP',
      choices: modified.services.map((svc, i) => ({
        name: `${svc.name} (${svc.type}${svc.version ? ` v${svc.version}` : ''}) — ${svc.actionDescription}`,
        value: i,
      })),
    })
    for (let i = 0; i < modified.services.length; i++) {
      if (toSkip.includes(i)) {
        modified.services[i]!.action = 'skip'
        modified.services[i]!.actionDescription = `Skipped by user`
      }
    }
  }

  if (what.includes('domains') && modified.domains.length > 0) {
    const toSkip = await promptCheckbox<number>({
      message: 'Select domains to SKIP',
      choices: modified.domains.map((d, i) => ({
        name: d.domain,
        value: i,
      })),
    })
    for (let i = 0; i < modified.domains.length; i++) {
      if (toSkip.includes(i)) {
        modified.domains[i]!.action = 'skip'
      }
    }
  }

  if (what.includes('steps')) {
    const skippableSteps = modified.steps.filter((s) => s.skippable)
    if (skippableSteps.length > 0) {
      const toSkip = await promptCheckbox<string>({
        message: 'Select steps to SKIP',
        choices: skippableSteps.map((s) => ({
          name: `[${s.order}] ${s.title}`,
          value: s.id,
        })),
      })
      for (const step of modified.steps) {
        if (toSkip.includes(step.id)) {
          step.skipped = true
        }
      }
    } else {
      info('No skippable steps available')
    }
  }

  // Re-display modified plan
  newline()
  info('Modified plan:')
  displayMigrationPlan(modified)

  return modified
}
