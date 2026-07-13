/**
 * Tool Registry
 *
 * Aggregates all tool modules and provides list/dispatch functions
 * for the MCP server.
 *
 * Supports category filtering via:
 *   --tools deployments,analytics,projects   (include only these)
 *   --tools all                              (include everything, default)
 *   TEMPS_MCP_TOOLS=deployments,analytics    (env var alternative)
 *
 * Available categories:
 *   projects, deployments, environments, domains, services, backups,
 *   monitors, containers, users, settings, api-keys, webhooks, audit,
 *   dns-providers, notifications, scans, custom-domains, errors,
 *   proxy-logs, dsn, ip-access, incidents, funnels, presets, platform,
 *   email-domains, email-providers, emails, load-balancer, notification-prefs,
 *   analytics
 */

import type { ToolDefinition, ToolResult } from '../types/index.js';

import { tools as projectsTools } from './projects.js';
import { tools as deploymentsTools } from './deployments.js';
import { tools as environmentsTools } from './environments.js';
import { tools as domainsTools } from './domains.js';
import { tools as servicesTools } from './services.js';
import { tools as backupsTools } from './backups.js';
import { tools as monitorsTools } from './monitors.js';
import { tools as containersTools } from './containers.js';
import { tools as usersTools } from './users.js';
import { tools as settingsTools } from './settings.js';
import { tools as apiKeysTools } from './api-keys.js';
import { tools as webhooksTools } from './webhooks.js';
import { tools as auditTools } from './audit.js';
import { tools as dnsProvidersTools } from './dns-providers.js';
import { tools as notificationsTools } from './notifications.js';
import { tools as scansTools } from './scans.js';
import { tools as customDomainsTools } from './custom-domains.js';
import { tools as errorsTools } from './errors.js';
import { tools as proxyLogsTools } from './proxy-logs.js';
import { tools as dsnTools } from './dsn.js';
import { tools as ipAccessTools } from './ip-access.js';
import { tools as incidentsTools } from './incidents.js';
import { tools as funnelsTools } from './funnels.js';
import { tools as presetsTools } from './presets.js';
import { tools as platformTools } from './platform.js';
import { tools as emailDomainsTools } from './email-domains.js';
import { tools as emailProvidersTools } from './email-providers.js';
import { tools as emailsTools } from './emails.js';
import { tools as loadBalancerTools } from './load-balancer.js';
import { tools as notificationPrefsTools } from './notification-prefs.js';
import { tools as analyticsTools } from './analytics.js';

// ── Category registry ────────────────────────────────────────────

/** Maps category name -> tool definitions */
const categoryRegistry: Record<string, ToolDefinition[]> = {
  projects: projectsTools,
  deployments: deploymentsTools,
  environments: environmentsTools,
  domains: domainsTools,
  services: servicesTools,
  backups: backupsTools,
  monitors: monitorsTools,
  containers: containersTools,
  users: usersTools,
  settings: settingsTools,
  'api-keys': apiKeysTools,
  webhooks: webhooksTools,
  audit: auditTools,
  'dns-providers': dnsProvidersTools,
  notifications: notificationsTools,
  scans: scansTools,
  'custom-domains': customDomainsTools,
  errors: errorsTools,
  'proxy-logs': proxyLogsTools,
  dsn: dsnTools,
  'ip-access': ipAccessTools,
  incidents: incidentsTools,
  funnels: funnelsTools,
  presets: presetsTools,
  platform: platformTools,
  'email-domains': emailDomainsTools,
  'email-providers': emailProvidersTools,
  emails: emailsTools,
  'load-balancer': loadBalancerTools,
  'notification-prefs': notificationPrefsTools,
  analytics: analyticsTools,
};

/** All available category names */
export const availableCategories = Object.keys(categoryRegistry);

// ── Filtering logic ──────────────────────────────────────────────

function parseToolFilter(): Set<string> | null {
  // 1. Check --tools CLI flag
  const args = process.argv;
  const flagIndex = args.indexOf('--tools');
  if (flagIndex !== -1 && args[flagIndex + 1]) {
    const value = args[flagIndex + 1];
    if (value === 'all') return null; // null = no filter
    return new Set(value.split(',').map((s) => s.trim().toLowerCase()));
  }

  // 2. Check TEMPS_MCP_TOOLS env var
  const envVal = process.env.TEMPS_MCP_TOOLS;
  if (envVal) {
    if (envVal === 'all') return null;
    return new Set(envVal.split(',').map((s) => s.trim().toLowerCase()));
  }

  // 3. Default: all tools
  return null;
}

function buildToolList(): ToolDefinition[] {
  const filter = parseToolFilter();

  // Validate filter categories
  if (filter) {
    const invalid = [...filter].filter((c) => !categoryRegistry[c]);
    if (invalid.length > 0) {
      console.error(
        `Warning: unknown tool categories: ${invalid.join(', ')}\n` +
          `Available: ${availableCategories.join(', ')}`,
      );
    }
  }

  const tools: ToolDefinition[] = [];
  const enabledCategories: string[] = [];

  for (const [category, categoryTools] of Object.entries(categoryRegistry)) {
    if (filter && !filter.has(category)) continue;
    tools.push(...categoryTools);
    enabledCategories.push(`${category}(${categoryTools.length})`);
  }

  if (filter) {
    console.error(`Tool categories enabled: ${enabledCategories.join(', ')}`);
  }

  return tools;
}

// ── Build the active tool set ────────────────────────────────────

const activeTools = buildToolList();

/** Build lookup map for O(1) dispatch */
const toolMap = new Map<string, ToolDefinition>();
for (const tool of activeTools) {
  if (toolMap.has(tool.name)) {
    throw new Error(`Duplicate tool name: ${tool.name}`);
  }
  toolMap.set(tool.name, tool);
}

// ── Public API ───────────────────────────────────────────────────

/**
 * List all available tools (for ListToolsRequest)
 */
export function listTools() {
  return {
    tools: activeTools.map((t) => ({
      name: t.name,
      description: t.description,
      inputSchema: t.inputSchema,
    })),
  };
}

/**
 * Call a tool by name (for CallToolRequest)
 */
export async function callTool(
  name: string,
  args: Record<string, unknown>,
): Promise<ToolResult> {
  const tool = toolMap.get(name);
  if (!tool) {
    return {
      content: [{ type: 'text', text: `Unknown tool: ${name}` }],
      isError: true,
    };
  }
  return tool.handler(args);
}

/** Total number of registered tools */
export const toolCount = activeTools.length;
