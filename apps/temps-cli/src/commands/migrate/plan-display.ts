// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Plan display — renders the migration plan in a human-readable format.
 *
 * Shows:
 * - Summary headline and overall risk
 * - Ordered execution steps with risk levels
 * - Environment variables (with secrets masked)
 * - Service plans with data implications
 * - Domain plans
 * - Unsupported features
 * - Manual actions required
 * - Critical warnings
 */

import chalk from 'chalk'
import {
  header,
  newline,
  keyValue,
  colors,
  icons,
  warning,
  info,
  box,
} from '../../ui/output.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import type { MigrationPlan, RiskLevel, MigrationStep, MigrationResult, StepResult } from './types.js'

// ---------------------------------------------------------------------------
// Risk level formatting
// ---------------------------------------------------------------------------

function riskBadge(risk: RiskLevel): string {
  switch (risk) {
    case 'none':
      return chalk.gray('NONE')
    case 'low':
      return chalk.green('LOW')
    case 'medium':
      return chalk.yellow('MEDIUM')
    case 'high':
      return chalk.red('HIGH')
    case 'critical':
      return chalk.bgRed.white(' CRITICAL ')
    default:
      return chalk.gray(risk)
  }
}

// ---------------------------------------------------------------------------
// Main display function
// ---------------------------------------------------------------------------

export function displayMigrationPlan(plan: MigrationPlan): void {
  // ─── Header ────────────────────────────────────────────
  newline()
  box(plan.summary.headline, 'Migration Plan')
  newline()

  keyValue('Source', `${plan.platform} → ${plan.sourceProjectName}`)
  keyValue('Target project', plan.project.name)
  keyValue('Preset', plan.project.preset)
  keyValue('Branch', plan.project.mainBranch)
  keyValue('Overall risk', riskBadge(plan.summary.overallRisk))

  if (plan.project.git) {
    keyValue('Git', `${plan.project.git.provider}: ${plan.project.git.owner}/${plan.project.git.repo}`)
  }

  // ─── Critical Warnings ─────────────────────────────────
  if (plan.summary.criticalWarnings.length > 0) {
    newline()
    header(`${icons.warning} Critical Warnings`)
    for (const w of plan.summary.criticalWarnings) {
      console.log(`  ${chalk.red('!')} ${chalk.yellow(w)}`)
    }
  }

  // ─── Execution Steps ───────────────────────────────────
  newline()
  header(`${icons.arrow} Execution Steps (${plan.steps.length})`)

  const stepColumns: TableColumn<MigrationStep>[] = [
    {
      header: '#',
      accessor: (s) => String(s.order),
      width: 4,
    },
    {
      header: 'Step',
      accessor: (s) => s.title,
      color: (v, s) => (s.skipped ? chalk.strikethrough.gray(v) : colors.bold(v)),
    },
    {
      header: 'Risk',
      accessor: (s) => s.risk.toUpperCase(),
      color: (v, s) => riskBadge(s.risk),
    },
    {
      header: 'Skip?',
      accessor: (s) => (s.skippable ? (s.skipped ? 'SKIPPED' : 'optional') : 'required'),
      color: (v, s) => (s.skipped ? chalk.gray(v) : s.skippable ? chalk.yellow(v) : chalk.green(v)),
    },
  ]

  printTable(plan.steps, stepColumns, { style: 'minimal' })

  // ─── Environment Variables ─────────────────────────────
  if (plan.envVars.length > 0) {
    newline()
    const activeEnvVars = plan.envVars.filter((e) => !e.skip)
    const skippedEnvVars = plan.envVars.filter((e) => e.skip)
    header(`${icons.key} Environment Variables (${activeEnvVars.length} active, ${skippedEnvVars.length} auto-managed)`)

    const envColumns: TableColumn<typeof plan.envVars[0]>[] = [
      {
        header: 'Key',
        accessor: (e) => e.key,
        color: (v, e) => (e.skip ? chalk.gray(v) : e.isSecret ? chalk.yellow(v) : v),
      },
      {
        header: 'Value',
        accessor: (e) => {
          if (e.skip) return chalk.gray('(auto-managed by service)')
          if (e.isSecret) return chalk.gray('••••••••')
          const val = e.value
          return val.length > 40 ? val.slice(0, 37) + '...' : val
        },
      },
      {
        header: 'Status',
        accessor: (e) => (e.skip ? 'skip' : e.isSecret ? 'secret' : 'plain'),
        color: (v) =>
          v === 'skip' ? chalk.gray(v) : v === 'secret' ? chalk.yellow(`${icons.lock} ${v}`) : chalk.green(v),
      },
    ]

    printTable(plan.envVars, envColumns, { style: 'minimal' })
  }

  // ─── Services ──────────────────────────────────────────
  if (plan.services.length > 0) {
    newline()
    header(`${icons.package} Services (${plan.services.length})`)

    for (const svc of plan.services) {
      const actionColor = svc.action === 'create' ? chalk.green : svc.action === 'skip' ? chalk.gray : chalk.yellow
      console.log(`  ${actionColor(svc.action.toUpperCase())} ${colors.bold(svc.name)} (${svc.type}${svc.version ? ` v${svc.version}` : ''})`)
      console.log(`    ${colors.muted(svc.actionDescription)}`)

      for (const impl of svc.dataImplications) {
        const icon =
          impl.severity === 'potential-data-loss'
            ? chalk.red('!!')
            : impl.severity === 'data-not-migrated'
              ? chalk.yellow('!')
              : impl.severity === 'warning'
                ? icons.warning
                : icons.info
        console.log(`    ${icon} ${impl.message}`)
        if (impl.recommendedAction) {
          console.log(`      ${colors.muted(`→ ${impl.recommendedAction}`)}`)
        }
      }
    }
  }

  // ─── Domains ───────────────────────────────────────────
  if (plan.domains.length > 0) {
    newline()
    header(`${icons.globe} Domains (${plan.domains.length})`)

    for (const d of plan.domains) {
      const actionColor = d.action === 'import' ? chalk.green : chalk.gray
      console.log(`  ${actionColor(d.action.toUpperCase())} ${colors.bold(d.domain)}`)
      console.log(`    ${colors.muted(d.actionDescription)}`)
    }
  }

  // ─── Unsupported Features ──────────────────────────────
  if (plan.unsupportedFeatures.length > 0) {
    newline()
    header(`${icons.warning} Unsupported Features (${plan.unsupportedFeatures.length})`)

    for (const uf of plan.unsupportedFeatures) {
      console.log(`  ${chalk.yellow('⚠')} ${colors.bold(uf.feature)}`)
      console.log(`    ${colors.muted(`Reason: ${uf.reason}`)}`)
      if (uf.alternative) {
        console.log(`    ${chalk.cyan('→')} ${uf.alternative}`)
      }
    }
  }

  // ─── Manual Actions ────────────────────────────────────
  if (plan.summary.manualActions.length > 0) {
    newline()
    header(`${icons.clock} Manual Actions Required`)

    for (const action of plan.summary.manualActions) {
      const timing =
        action.timing === 'before'
          ? chalk.yellow('BEFORE')
          : action.timing === 'after'
            ? chalk.cyan('AFTER')
            : chalk.magenta('WITHIN HOURS')
      console.log(`  ${timing} ${action.description}`)
      console.log(`    ${colors.muted(`Reason: ${action.reason}`)}`)
    }
  }

  // ─── Summary counts ────────────────────────────────────
  newline()
  header('Summary')
  keyValue('Environment variables', plan.summary.counts.envVars)
  keyValue('Services', plan.summary.counts.services)
  keyValue('Domains', plan.summary.counts.domains)
  keyValue('Unsupported features', plan.unsupportedFeatures.length)
  keyValue('Manual actions', plan.summary.manualActions.length)
  newline()
}

// ---------------------------------------------------------------------------
// Result display
// ---------------------------------------------------------------------------

export function displayMigrationResult(result: MigrationResult): void {
  newline()

  if (result.success) {
    box(
      `Project created successfully in ${formatDuration(result.durationMs)}`,
      `${icons.success} Migration Complete`
    )
  } else {
    box(
      `Migration completed with errors in ${formatDuration(result.durationMs)}`,
      `${icons.error} Migration Failed`
    )
  }

  newline()

  if (result.projectId) {
    keyValue('Project ID', result.projectId)
  }
  if (result.projectSlug) {
    keyValue('Project slug', result.projectSlug)
  }
  if (result.environmentId) {
    keyValue('Environment ID', result.environmentId)
  }

  // Step results
  newline()
  header('Step Results')

  const resultColumns: TableColumn<StepResult>[] = [
    {
      header: 'Step',
      accessor: (s) => s.title,
      color: (v, s) => (s.skipped ? chalk.gray(v) : s.success ? v : chalk.red(v)),
    },
    {
      header: 'Status',
      accessor: (s) => (s.skipped ? 'SKIPPED' : s.success ? 'OK' : 'FAILED'),
      color: (v, s) =>
        s.skipped ? chalk.gray(v) : s.success ? chalk.green(v) : chalk.red(v),
    },
    {
      header: 'Duration',
      accessor: (s) => formatDuration(s.durationMs),
      color: (v) => colors.muted(v),
    },
    {
      header: 'Message',
      accessor: (s) => s.message,
      color: (v, s) => (s.success ? colors.muted(v) : chalk.red(v)),
    },
  ]

  printTable(result.stepResults, resultColumns, { style: 'minimal' })

  // Show failed steps prominently
  const failedSteps = result.stepResults.filter((s) => !s.success && !s.skipped)
  if (failedSteps.length > 0) {
    newline()
    warning(`${failedSteps.length} step(s) failed:`)
    for (const step of failedSteps) {
      console.log(`  ${icons.error} ${colors.bold(step.title)}: ${chalk.red(step.message)}`)
    }
  }

  // Show created resources
  const createdResources = result.stepResults.filter((s) => s.createdResource)
  if (createdResources.length > 0) {
    newline()
    info('Created resources:')
    for (const step of createdResources) {
      const r = step.createdResource!
      console.log(`  ${icons.success} ${r.type} "${r.name}" (ID: ${r.id})`)
    }
  }

  newline()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`
  const secs = Math.floor(ms / 1000)
  if (secs < 60) return `${secs}s`
  const mins = Math.floor(secs / 60)
  const remainSecs = secs % 60
  return `${mins}m ${remainSecs}s`
}
