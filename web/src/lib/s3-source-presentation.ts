// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export function shouldShowS3SourceHeaderAction(
  isLoading: boolean,
  sourceCount: number
) {
  return !isLoading && sourceCount > 0
}

export type WorkerCredentialsPolicy = 'scoped' | 'allow_full'

export interface WorkerCredentialsPresentation {
  label: string
  description: string
  /** Worker nodes may hold this source's own credential. */
  warning: boolean
}

/**
 * What worker nodes receive for a source's backups and restores (ADR-093).
 * A Temps Cloud managed source is always scoped: Cloud mints every worker
 * job's credential and its own token never leaves the control plane.
 */
export function workerCredentialsPresentation(
  policy: WorkerCredentialsPolicy | undefined,
  managedByCloud: boolean
): WorkerCredentialsPresentation {
  if (managedByCloud) {
    return {
      label: 'Scoped by Temps Cloud',
      description:
        'Each worker job gets a short-lived credential for its own service’s prefix, minted by Temps Cloud.',
      warning: false,
    }
  }
  if (policy === 'allow_full') {
    return {
      label: 'Full credentials allowed',
      description:
        'If this store cannot mint a scoped credential, worker nodes receive this source’s own key and can read and delete every backup in the bucket.',
      warning: true,
    }
  }
  return {
    label: 'Scoped per job',
    description:
      'Each worker job gets a short-lived credential for its own service’s prefix. If this store cannot mint one, the job is refused.',
    warning: false,
  }
}
