// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// The generated type, imported directly: this module is pure and must not
// pull a hook (and therefore React) into a unit test.
import type { DockerSocketCapability } from '@/api/client/types.gen'

/** Name the API uses for the control plane's own host in `nodes`. */
export const CONTROL_PLANE_NODE_NAME = 'control-plane'

/** One label for the badge, the alert and the tooltip, so they cannot drift. */
export const HOST_DOCKER_ACCESS_LABEL = 'Host Docker access'

/**
 * Short form of {@link HOST_DOCKER_ACCESS_LABEL} for narrow viewports.
 *
 * The badge is the only place the console says a project is root-equivalent on
 * its host, so it must survive a phone-width header rather than being hidden
 * with the other secondary badges. "Host root" is the shortest phrasing that
 * still says the dangerous part out loud; the full label and the reason stay
 * in the accessible name and the tooltip.
 */
export const HOST_DOCKER_ACCESS_SHORT_LABEL = 'Host root'

/**
 * What the grant actually does, in one sentence, for an operator who has never
 * heard of it. Said in full wherever it is offered — nobody reading this has a
 * support channel to ask what "host Docker access" means.
 */
export const HOST_DOCKER_ACCESS_EXPLANATION =
  "Mounts /var/run/docker.sock into this project's containers so it can manage containers on its host — for operator-owned infrastructure services only. A granted project is root-equivalent on every host that grants it."

/**
 * Whether a project holds host Docker access, cannot be told, or plainly does
 * not.
 *
 * `unknown` is its own state on purpose: the API attaches `docker_socket` only
 * to the single-project detail responses, so a missing field means "this
 * response did not compute it", never "not granted". Rendering a "not granted"
 * onboarding prompt from a list payload would tell the operator something the
 * server never said.
 */
export type DockerSocketState = 'granted' | 'not-granted' | 'unknown'

export interface DockerSocketDescription {
  state: DockerSocketState
  /** Short label for a badge or an alert title. */
  label: string
  /** A full sentence: where it is granted, or what is missing. */
  detail: string
  /** Hosts that grant it — worker node names, plus `control-plane`. */
  nodes: string[]
}

/** Human list of granting hosts: "worker-1", "worker-1 and worker-2", … */
function joinNodes(nodes: string[]): string {
  if (nodes.length === 0) return ''
  if (nodes.length === 1) return nodes[0]
  return `${nodes.slice(0, -1).join(', ')} and ${nodes[nodes.length - 1]}`
}

/**
 * Turn the capability object into the three strings every surface needs.
 *
 * `granted` is read from the field, never inferred from a non-empty `nodes`:
 * the server owns that rule, and a client that re-derived it would disagree
 * with the deployer the moment the rule changed.
 */
export function describeDockerSocket(
  capability: DockerSocketCapability | null | undefined
): DockerSocketDescription {
  if (capability == null) {
    return {
      state: 'unknown',
      label: HOST_DOCKER_ACCESS_LABEL,
      detail: 'This response does not report host Docker access.',
      nodes: [],
    }
  }

  const nodes = capability.nodes ?? []

  if (capability.granted) {
    return {
      state: 'granted',
      label: HOST_DOCKER_ACCESS_LABEL,
      detail: nodes.length
        ? `Granted on ${joinNodes(nodes)}.`
        : 'Granted on this installation.',
      nodes,
    }
  }

  return {
    state: 'not-granted',
    label: HOST_DOCKER_ACCESS_LABEL,
    // The server's `reason` names the exact variable, value and process to
    // restart; only fall back when an older server sends none.
    detail:
      capability.reason ||
      'No host grants this project access to the Docker socket.',
    nodes,
  }
}

/**
 * Whether to render the onboarding alert.
 *
 * Two independent gates. The capability must say "not granted" — `unknown`
 * (a list payload, or a server that predates the field) is silence, not a
 * problem to advertise. And the viewer must be able to act on it: the remedy
 * is an environment variable on a host, and the page that shows which hosts
 * exist needs node-management permission, so showing it to a project user
 * would be an advertised remedy that refuses whoever follows it.
 */
export function shouldShowDockerSocketOnboarding(
  capability: DockerSocketCapability | null | undefined,
  canManageNodes: boolean
): boolean {
  return (
    canManageNodes && describeDockerSocket(capability).state === 'not-granted'
  )
}

/** Whether to render the header badge. */
export function shouldShowDockerSocketBadge(
  capability: DockerSocketCapability | null | undefined
): boolean {
  return describeDockerSocket(capability).state === 'granted'
}
