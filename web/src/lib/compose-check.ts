// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  ComposeCheckErrorClass,
  ComposeHostStatusResponse,
  ComposeValidationReportResponse,
} from '@/api/client'

const ERROR_CLASS_LABELS: Record<ComposeCheckErrorClass, string> = {
  interpolation: 'Variable interpolation',
  policy_violation: 'Security policy',
  invalid_path: 'File path',
  invalid_yaml: 'Invalid Compose file',
  invalid_override: 'Project override',
  unsupported_compose_version: 'Docker Compose too old',
  config_resolution: 'Compose configuration',
  compose_unavailable: 'Docker Compose unavailable',
  internal: 'Internal error',
}

const ERROR_CLASS_HINTS: Record<ComposeCheckErrorClass, string> = {
  interpolation:
    'Set the variable on the environment, or give it a default in the Compose file: ${VAR:-value}.',
  policy_violation:
    'Correct the value in the Compose file. Only instance administrators can change security checks.',
  invalid_path:
    'Use a relative path inside the repository that does not name a Temps-generated file.',
  invalid_yaml: 'Fix the YAML syntax in the Compose file.',
  invalid_override:
    'Keep only the fields the override may change, or clear the override.',
  unsupported_compose_version:
    'Upgrade the Docker Compose plugin on the Temps host to v2.35.0 or newer (check with `docker compose version`), then restart Temps.',
  config_resolution:
    'Run `docker compose config` locally against the same files to see the full error.',
  compose_unavailable:
    'Install the Docker Compose v2 plugin on the Temps host and verify it with `docker compose version`, then restart Temps.',
  internal:
    'Retry the check. If it keeps failing, check the Temps server logs.',
}

export function composeErrorClassLabel(
  errorClass: ComposeCheckErrorClass | null | undefined
): string {
  return errorClass ? ERROR_CLASS_LABELS[errorClass] : 'Compose check failed'
}

export function composeErrorClassHint(
  errorClass: ComposeCheckErrorClass | null | undefined
): string | null {
  return errorClass ? ERROR_CLASS_HINTS[errorClass] : null
}

/**
 * Operator-facing warning when the host's Docker Compose CLI cannot run the
 * checks Compose deployments need. `null` while the startup probe has not
 * finished, or when the CLI is supported.
 */
export function composeHostWarning(
  status: ComposeHostStatusResponse | null | undefined
): string | null {
  if (!status?.probed || (status.available && status.supported)) {
    return null
  }
  if (status.problem) {
    return status.problem
  }
  return status.available
    ? `Docker Compose ${status.version ?? ''} is older than v${status.minimum_version}; Compose deployments that use an override, include or extends will fail.`.replace(
        /\s+/g,
        ' '
      )
    : 'Docker Compose v2 is not installed on the Temps host; Compose deployments will fail.'
}

export interface ComposeValidationWarning {
  title: string
  detail: string
  hint: string | null
}

/** Warning for a project whose last stored Compose check failed. */
export function composeValidationWarning(
  report: ComposeValidationReportResponse | null | undefined
): ComposeValidationWarning | null {
  if (!report || report.passed) {
    return null
  }
  return {
    title: report.outdated
      ? 'The last Compose check failed under an older Temps version'
      : `The next deployment would fail: ${composeErrorClassLabel(report.error_class).toLowerCase()}`,
    detail: report.error ?? 'The Compose configuration check failed.',
    hint: composeErrorClassHint(report.error_class),
  }
}

/** A shell-pasteable rendering of a sanitized `docker compose` argv. */
export function composeCommandLine(argv: string[]): string {
  return argv
    .map((arg) =>
      /^[\w@%+=:,./-]+$/.test(arg) ? arg : `'${arg.replace(/'/g, `'\\''`)}'`
    )
    .join(' ')
}
