// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Registers the table-declared API commands (each area's `endpoints.ts`).
 * Runs after every area registered its own commands, so a table adds to an
 * existing group (`temps backups ...`) instead of replacing it.
 */

import type { Command } from 'commander'
import { registerAgentsEndpoints } from './agents/endpoints.js'
import { registerAiEndpoints } from './ai/endpoints.js'
import { registerAlarmsEndpoints } from './alarms/endpoints.js'
import { registerAnalyticsEndpoints } from './analytics/endpoints.js'
import { registerApikeysEndpoints } from './apikeys/endpoints.js'
import { registerBackupsEndpoints } from './backups/endpoints.js'
import { registerCloudEndpoints } from './cloud/endpoints.js'
import { registerContainersEndpoints } from './containers/endpoints.js'
import { registerCustomDomainsEndpoints } from './custom-domains/endpoints.js'
import { registerDeployEndpoints } from './deploy/endpoints.js'
import { registerDnsProvidersEndpoints } from './dns-providers/endpoints.js'
import { registerDomainsEndpoints } from './domains/endpoints.js'
import { registerEmailProvidersEndpoints } from './email-providers/endpoints.js'
import { registerEmailsEndpoints } from './emails/endpoints.js'
import { registerEnvironmentsEndpoints } from './environments/endpoints.js'
import { registerErrorsEndpoints } from './errors/endpoints.js'
import { registerFlagsEndpoints } from './flags/endpoints.js'
import { registerFunnelsEndpoints } from './funnels/endpoints.js'
import { registerGlobalLogsEndpoints } from './global-logs/endpoints.js'
import { registerIncidentsEndpoints } from './incidents/endpoints.js'
import { registerIpAccessEndpoints } from './ip-access/endpoints.js'
import { registerMonitorsEndpoints } from './monitors/endpoints.js'
import { registerNetworkPolicyEndpoints } from './network-policy/endpoints.js'
import { registerNotificationsEndpoints } from './notifications/endpoints.js'
import { registerOtelEndpoints } from './otel/endpoints.js'
import { registerPluginEndpoints } from './plugin/endpoints.js'
import { registerProjectsEndpoints } from './projects/endpoints.js'
import { registerProvidersEndpoints } from './providers/endpoints.js'
import { registerProxyLogsEndpoints } from './proxy-logs/endpoints.js'
import { registerRevenueEndpoints } from './revenue/endpoints.js'
import { registerSandboxEndpoints } from './sandbox/endpoints.js'
import { registerServicesEndpoints } from './services/endpoints.js'
import { registerSettingsEndpoints } from './settings/endpoints.js'
import { registerShieldEndpoints } from './shield/endpoints.js'
import { registerStatusPagesEndpoints } from './status-pages/endpoints.js'
import { registerTemplatesEndpoints } from './templates/endpoints.js'
import { registerTokensEndpoints } from './tokens/endpoints.js'
import { registerTracesEndpoints } from './traces/endpoints.js'
import { registerUsersEndpoints } from './users/endpoints.js'
import { registerVaultEndpoints } from './vault/endpoints.js'
import { registerWorkflowsEndpoints } from './workflows/endpoints.js'

export function registerEndpointCommands(program: Command): void {
  registerAgentsEndpoints(program)
  registerAiEndpoints(program)
  registerAlarmsEndpoints(program)
  registerAnalyticsEndpoints(program)
  registerApikeysEndpoints(program)
  registerBackupsEndpoints(program)
  registerCloudEndpoints(program)
  registerContainersEndpoints(program)
  registerCustomDomainsEndpoints(program)
  registerDeployEndpoints(program)
  registerDnsProvidersEndpoints(program)
  registerDomainsEndpoints(program)
  registerEmailProvidersEndpoints(program)
  registerEmailsEndpoints(program)
  registerEnvironmentsEndpoints(program)
  registerErrorsEndpoints(program)
  registerFlagsEndpoints(program)
  registerFunnelsEndpoints(program)
  registerGlobalLogsEndpoints(program)
  registerIncidentsEndpoints(program)
  registerIpAccessEndpoints(program)
  registerMonitorsEndpoints(program)
  registerNetworkPolicyEndpoints(program)
  registerNotificationsEndpoints(program)
  registerOtelEndpoints(program)
  registerPluginEndpoints(program)
  registerProjectsEndpoints(program)
  registerProvidersEndpoints(program)
  registerProxyLogsEndpoints(program)
  registerRevenueEndpoints(program)
  registerSandboxEndpoints(program)
  registerServicesEndpoints(program)
  registerSettingsEndpoints(program)
  registerShieldEndpoints(program)
  registerStatusPagesEndpoints(program)
  registerTemplatesEndpoints(program)
  registerTokensEndpoints(program)
  registerTracesEndpoints(program)
  registerUsersEndpoints(program)
  registerVaultEndpoints(program)
  registerWorkflowsEndpoints(program)
}
