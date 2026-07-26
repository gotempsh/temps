/**
 * Import plan display — renders a `CreatePlanResponse` (real ImportPlan +
 * ValidationReport from the temps-import backend) in a readable, colorful
 * format: summary, risk, warnings, services with data implications, domains,
 * environment variables (secrets already masked server-side), manual
 * actions, unsupported features, and cost analysis when present.
 */

import chalk from 'chalk'
import { header, newline, keyValue, colors, icons, box } from '../../ui/output.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import type {
  ImportPlan,
  ValidationReport,
  RiskLevel,
  ServicePlan,
  DomainPlan,
  EnvironmentVariable,
  ManualAction,
  UnsupportedFeature,
  CostAnalysis,
} from '../../api/types.gen.js'

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

export function displayImportPlan(plan: ImportPlan, validation: ValidationReport): void {
  const summary = plan.summary

  newline()
  box(summary.headline, `${plan.source} → temps`)
  newline()

  keyValue('Target project', plan.project.name)
  keyValue('Environment', plan.environment.name)
  keyValue('Overall risk', riskBadge(summary.overall_risk))
  keyValue('Complexity', plan.metadata.complexity)
  if (plan.deployment.git) {
    keyValue('Git source', `${plan.deployment.git.owner}/${plan.deployment.git.repo}@${plan.deployment.git.branch}`)
  } else {
    keyValue('Deploy from', plan.deployment.image)
  }

  const counts = summary.resource_counts
  keyValue(
    'Resources',
    `${counts.services} service(s), ${counts.domains} domain(s), ${counts.environment_variables} env var(s)`,
  )

  if (validation.overall_status !== 'passed') {
    newline()
    warningBlock(
      `Validation: ${validation.overall_status}`,
      validation.results
        .filter((r) => !r.passed)
        .map((r) => `${r.rule_name}: ${r.message}`),
    )
  }

  if (summary.critical_warnings && summary.critical_warnings.length > 0) {
    newline()
    warningBlock(`${icons.warning} Critical Warnings`, summary.critical_warnings)
  }

  if (plan.metadata.warnings && plan.metadata.warnings.length > 0) {
    newline()
    warningBlock(`${icons.warning} Warnings`, plan.metadata.warnings)
  }

  if (plan.services && plan.services.length > 0) {
    displayServices(plan.services)
  }

  if (plan.domains && plan.domains.length > 0) {
    displayDomains(plan.domains)
  }

  displayEnvVars(plan.deployment.env_vars)

  if (plan.cost_analysis) {
    displayCostAnalysis(plan.cost_analysis)
  }

  if (summary.manual_actions_required && summary.manual_actions_required.length > 0) {
    displayManualActions(summary.manual_actions_required)
  }

  if (summary.unsupported_features && summary.unsupported_features.length > 0) {
    displayUnsupportedFeatures(summary.unsupported_features)
  }

  newline()
}

function warningBlock(title: string, lines: string[]): void {
  header(title)
  for (const line of lines) {
    console.log(`  ${chalk.yellow('!')} ${chalk.yellow(line)}`)
  }
}

function displayServices(services: ServicePlan[]): void {
  newline()
  header(`${icons.package} Services (${services.length})`)

  for (const svc of services) {
    const actionColor = svc.action === 'create' ? chalk.green : svc.action === 'skip' ? chalk.gray : chalk.yellow
    console.log(
      `  ${actionColor(svc.action.toUpperCase())} ${colors.bold(svc.name)} (${svc.service_type}${
        svc.version ? ` v${svc.version}` : ''
      })`,
    )
    console.log(`    ${colors.muted(svc.action_description)}`)
    for (const implication of svc.data_implications ?? []) {
      const sevColor =
        implication.severity === 'potential-data-loss'
          ? chalk.red
          : implication.severity === 'data-not-migrated'
            ? chalk.yellow
            : chalk.gray
      console.log(`    ${sevColor(`[${implication.severity}]`)} ${implication.message}`)
      if (implication.recommended_action) {
        console.log(`      ${colors.muted(icons.arrow)} ${colors.muted(implication.recommended_action)}`)
      }
    }
  }
}

function displayDomains(domains: DomainPlan[]): void {
  newline()
  header(`${icons.globe} Domains (${domains.length})`)

  const columns: TableColumn<DomainPlan>[] = [
    { header: 'Domain', accessor: (d) => d.domain, color: (v) => colors.bold(v) },
    {
      header: 'Action',
      accessor: (d) => d.action,
      color: (v) => (v === 'import' ? chalk.green(v) : chalk.yellow(v)),
    },
    { header: 'Detail', accessor: (d) => d.replacement ?? d.action_description, color: (v) => colors.muted(v) },
  ]
  printTable(domains, columns, { style: 'minimal' })
}

function displayEnvVars(envVars: EnvironmentVariable[]): void {
  if (envVars.length === 0) return
  newline()
  const secretCount = envVars.filter((e) => e.is_secret).length
  header(`${icons.key} Environment Variables (${envVars.length}, ${secretCount} secret)`)

  const columns: TableColumn<EnvironmentVariable>[] = [
    { header: 'Key', accessor: (e) => e.key, color: (v, e) => (e.is_secret ? chalk.yellow(v) : v) },
    {
      header: 'Value',
      accessor: (e) => (e.is_secret ? '***' : e.value.length > 40 ? e.value.slice(0, 37) + '...' : e.value),
      color: (v, e) => (e.is_secret ? chalk.gray(v) : v),
    },
  ]
  printTable(envVars, columns, { style: 'minimal' })
}

function displayCostAnalysis(cost: CostAnalysis): void {
  newline()
  header(`${icons.info} Cost Analysis`)
  if (cost.current_monthly_usd != null) {
    keyValue('Current estimated cost', `$${cost.current_monthly_usd.toFixed(2)}/mo`)
  }
  const savings = cost.recommendation.monthly_savings_usd
  if (savings != null) {
    keyValue('Estimated temps savings', chalk.green(`$${savings.toFixed(2)}/mo`))
  }
  for (const note of cost.notes) {
    console.log(`  ${colors.muted(icons.bullet)} ${colors.muted(note)}`)
  }
}

function displayManualActions(actions: ManualAction[]): void {
  newline()
  header(`${icons.warning} Manual Actions Required (${actions.length})`)
  for (const action of actions) {
    console.log(`  ${chalk.yellow(`[${action.timing}]`)} ${action.description}`)
    console.log(`    ${colors.muted(action.reason)}`)
  }
}

function displayUnsupportedFeatures(features: UnsupportedFeature[]): void {
  newline()
  header(`${icons.cross} Not Migrated (${features.length})`)
  for (const feature of features) {
    console.log(`  ${chalk.gray(feature.feature)}: ${colors.muted(feature.reason)}`)
    if (feature.alternative) {
      console.log(`    ${colors.muted(icons.arrow)} ${colors.muted(feature.alternative)}`)
    }
  }
}
