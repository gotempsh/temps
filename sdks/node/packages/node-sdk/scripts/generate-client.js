const fs = require('fs');

// Read the SDK file and extract all function names
const sdkContent = fs.readFileSync('./src/client/sdk.gen.ts', 'utf-8');
const functionMatches = sdkContent.matchAll(/^export const (\w+) =/gm);
const functions = Array.from(functionMatches, m => m[1]).sort();

// Group functions by category
const categories = {
  'Platform': ['getPlatformInfo'],
  'MCP': ['listClients', 'addClient', 'removeClient', 'connectClient'],
  'Analytics & Metrics': [
    'getAnalyticsScript', 'getPerformanceScript', 'recordEventMetrics', 'recordSpeedMetrics', 'updateSpeedMetrics',
    'getBrowsers', 'getEventsCount', 'getAnalyticsMetrics', 'getPathVisitors', 'getReferrers',
    'getSessionDetails', 'getSessionEvents', 'getSessionLogs', 'getSessionMetrics',
    'getStatusCodes', 'getViewsOverTime', 'getVisitorLocations', 'getVisitors', 'getVisitorDetails',
    'enrichVisitor', 'getVisitorSessions', 'getSpeedMetricsOverTime', 'getSpeedPerformanceMetrics',
    'getDeploymentMetricsHistogram', 'getTodayErrorsCount', 'getProjectErrorStats', 'getHourlyVisitorStats',
    'getProjectVisitorStats', 'getTotalVisitorStats', 'getProjectStats'
  ],
  'API Keys': [
    'listApiKeys', 'createApiKey', 'getApiKeyPermissions', 'getApiKey', 'updateApiKey',
    'deleteApiKey', 'activateApiKey', 'deactivateApiKey'
  ],
  'Audit Logs': ['listAuditLogs', 'getAuditLog'],
  'Authentication': ['initAuth', 'authStatus', 'verifyMfaChallenge', 'cliLogin', 'login', 'logout', 'renewToken'],
  'Backups': [
    'listS3Sources', 'createS3Source', 'deleteS3Source', 'getS3Source', 'updateS3Source',
    'listSourceBackups', 'runBackupForSource', 'listBackupSchedules', 'createBackupSchedule',
    'deleteBackupSchedule', 'getBackupSchedule', 'listBackupsForSchedule', 'disableBackupSchedule',
    'enableBackupSchedule', 'getBackup'
  ],
  'Dev Projects': [
    'getDevProjects', 'createDevProject', 'deleteDevProject', 'getDevProject',
    'buildDevContainer', 'pullDevProject', 'startDevContainer', 'stopDevContainer', 'devTerminalWs'
  ],
  'File Operations': [
    'deleteFileOrDirectory', 'createDirectory', 'readFile', 'createFile', 'writeFile',
    'listDirectory', 'getFile'
  ],
  'Git Operations': [
    'gitAdd', 'getBranches', 'createBranch', 'switchBranch', 'gitCommit', 'getGitLog',
    'gitPull', 'gitPush', 'gitRemove', 'getGitStatus', 'gitUnstage'
  ],
  'Domains': [
    'listDomains', 'createDomain', 'getDomainByHost', 'deleteDomain', 'getDomainById',
    'provisionDomain', 'renewDomain', 'checkDomainStatus', 'completeDnsChallenge',
    'getCustomDomains', 'createCustomDomain', 'deleteCustomDomain', 'updateCustomDomain',
    'checkCustomDomainConfiguration', 'provisionCertificate', 'provisionLbCertificate', 'renewCertificate'
  ],
  'Services': [
    'listServices', 'createService', 'getServiceBySlug', 'listProjectServices',
    'getProjectServiceEnvironmentVariables', 'getServiceTypes', 'getServiceTypeParameters',
    'deleteService', 'getService', 'updateService', 'listServiceProjects',
    'linkServiceToProject', 'unlinkServiceFromProject', 'getServiceEnvironmentVariables',
    'getServiceEnvironmentVariable', 'startService', 'stopService'
  ],
  'GitHub Integration': [
    'getAllGithubApps', 'githubCallback', 'redirectToGithubInstall', 'getAllGithubInstallations',
    'deleteGithubInstallation', 'syncGithubInstallation', 'getAllGithubRepos',
    'getGithubRepoByOwnerName', 'getRepoBranches', 'getGithubRepoPreset', 'getRepoSources',
    'githubAppCallback', 'updateGithubRepo', 'githubWebhook'
  ],
  'Routes': ['listRoutes', 'createRoute', 'deleteRoute', 'getRoute', 'updateRoute'],
  'Logs': [
    'getLogs', 'getLogById', 'streamLogs', 'getContainerLogs', 'getPipelineLogs',
    'getDeploymentStageLogs', 'tailDeploymentStageLogs', 'ingestLogs', 'ingestTraces'
  ],
  'Notifications': [
    'listProviders', 'createProvider', 'createEmailProvider', 'updateEmailProvider',
    'createSlackProvider', 'updateSlackProvider', 'deleteProvider', 'getProvider',
    'updateProvider', 'testProvider'
  ],
  'Payment': [
    'listPaymentProviders', 'createPaymentProvider', 'deletePaymentProvider',
    'getPaymentProvider', 'updatePaymentProvider', 'setDefaultPaymentProvider',
    'testPaymentProvider', 'getTotalRevenue', 'getProjectRevenueStats',
    'getPaymentEnvironmentVariables', 'getPaymentMetrics', 'getProducts',
    'createProduct', 'deleteProduct', 'getProduct', 'updateProduct',
    'getProjectPaymentProvider', 'setProjectPaymentProvider', 'getTodayRevenue',
    'getPaymentSettings'
  ],
  'Projects': [
    'getProjects', 'createProject', 'getProjectBySlug', 'createProjectFromTemplate',
    'deleteProject', 'getProject', 'updateProject', 'getProjectDeployments',
    'getLastDeployment', 'getProjectPipelines', 'triggerProjectPipeline',
    'updateAutomaticDeploy', 'updateDeploymentSettings', 'manualDockerDeployment',
    'manualStaticDeployment', 'updateProjectSettings', 'getProjectFavicon'
  ],
  'Deployments': [
    'getDeployment', 'pauseDeployment', 'resumeDeployment', 'getDeploymentStages',
    'teardownDeployment', 'rollbackToDeployment'
  ],
  'Environment Variables': [
    'getEnvironmentVariables', 'createEnvironmentVariable', 'getEnvironmentVariableValue',
    'deleteEnvironmentVariable', 'updateEnvironmentVariable'
  ],
  'Environments': [
    'getEnvironments', 'createEnvironment', 'getEnvironment', 'getEnvironmentDomains',
    'addEnvironmentDomain', 'deleteEnvironmentDomain', 'updateEnvironmentSettings',
    'teardownEnvironment', 'setEnvironmentMode', 'getEnvironmentCrons', 'getCronById',
    'getCronExecutions'
  ],
  'Webhooks': [
    'listWebhooks', 'createWebhook', 'getWebhookLogs', 'retryWebhook',
    'getWebhookLog', 'deleteWebhook', 'getWebhook', 'updateWebhook'
  ],
  'Feature Flags': ['getFeatureFlags', 'updateAutomaticAnalytics'],
  'Funnels': ['listFunnels', 'createFunnel', 'deleteFunnel', 'updateFunnel', 'getFunnelMetrics'],
  'OpenTelemetry': [
    'getOpentelemetryLogs', 'getOpentelemetryTraces', 'getTracePercentiles', 'getTraceDetails'
  ],
  'Templates': ['getTemplates', 'getTemplateByName', 'getTemplatePreview'],
  'Users': [
    'getCurrentUser', 'listUsers', 'createUser', 'updateSelf', 'disableMfa',
    'setupMfa', 'verifyAndEnableMfa', 'deleteUser', 'updateUser', 'restoreUser',
    'assignRole', 'removeRole'
  ],
  'Preferences': ['deletePreferences', 'getPreferences', 'updatePreferences'],
  'Setup': ['setupStatus'],
  'WebSocket': ['wsHandler']
};

// Generate method definitions
let methods = [];
for (const [category, funcs] of Object.entries(categories)) {
  const categoryMethods = funcs
    .filter(f => functions.includes(f))
    .map(f => {
      // Check if function requires parameters by looking for required params
      const funcRegex = new RegExp(`export const ${f} = .+?\\(options(\\?)?:`);
      const match = sdkContent.match(funcRegex);
      const isOptional = match && match[1] === '?';

      return `  ${f} = (options${isOptional ? '?' : ''}: Parameters<typeof sdk.${f}>[0]) =>
    sdk.${f}({ ...options, client: this.client });`;
    });

  if (categoryMethods.length > 0) {
    methods.push(`  // ${category}`);
    methods.push(...categoryMethods);
    methods.push('');
  }
}

// Generate the complete TypeScript file
const clientClass = `import { createClient, createConfig } from './client/client';
import type { Client } from './client/client';
import * as sdk from './client/sdk.gen';

export * from './client/types.gen';

export interface TempsClientConfig {
  baseUrl: string;
  apiKey?: string;
}

export class TempsClient {
  private client: Client;

  constructor(config: TempsClientConfig) {
    const clientConfig = createConfig({
      baseUrl: config.baseUrl,
      headers: config.apiKey ? {
        Authorization: \`Bearer \${config.apiKey}\`
      } : undefined
    });

    this.client = createClient(clientConfig);
  }

${methods.join('\n')}
  // Direct client access for advanced usage
  get rawClient() {
    return this.client;
  }
}

export default TempsClient;`;

fs.writeFileSync('./src/index.ts', clientClass);
console.log(`Generated client class with ${functions.length} methods`);
