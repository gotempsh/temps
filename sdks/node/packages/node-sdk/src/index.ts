import { createClient, createConfig } from './client/client';
import type { Client } from './client/client';
import * as sdk from './client/sdk.gen';

export * from './client/types.gen';
export * as ErrorTracking from './errors';

export interface TempsClientConfig {
  baseUrl: string;
  apiKey?: string;
}

export class TempsClient {
  private client: Client;

  // Namespace properties
  apiKeys: APIKeys;
  analytics: Analytics;
  auditLogs: AuditLogs;
  authentication: Authentication;
  backups: Backups;
  crons: Crons;
  deployments: Deployments;
  develop: Develop;
  domains: Domains;
  email: Email;
  externalServices: ExternalServices;
  featureFlags: FeatureFlags;
  files: Files;
  funnels: Funnels;
  github: Github;
  loadBalancer: LoadBalancer;
  logs: Logs;
  mcp: MCP;
  metrics: Metrics;
  notifications: Notifications;
  opentelemetry: OpenTelemetry;
  payments: Payments;
  pipelines: Pipelines;
  platform: Platform;
  projects: Projects;
  speedInsights: SpeedInsights;
  users: Users;
  websocket: WebSocket;

  constructor(config: TempsClientConfig) {
    const clientConfig = createConfig({
      baseUrl: config.baseUrl,
      headers: config.apiKey ? {
        Authorization: `Bearer ${config.apiKey}`
      } : undefined
    });

    this.client = createClient(clientConfig);

    // Initialize namespaces
    this.apiKeys = new APIKeys(this.client);
    this.analytics = new Analytics(this.client);
    this.auditLogs = new AuditLogs(this.client);
    this.authentication = new Authentication(this.client);
    this.backups = new Backups(this.client);
    this.crons = new Crons(this.client);
    this.deployments = new Deployments(this.client);
    this.develop = new Develop(this.client);
    this.domains = new Domains(this.client);
    this.email = new Email(this.client);
    this.externalServices = new ExternalServices(this.client);
    this.featureFlags = new FeatureFlags(this.client);
    this.files = new Files(this.client);
    this.funnels = new Funnels(this.client);
    this.github = new Github(this.client);
    this.loadBalancer = new LoadBalancer(this.client);
    this.logs = new Logs(this.client);
    this.mcp = new MCP(this.client);
    this.metrics = new Metrics(this.client);
    this.notifications = new Notifications(this.client);
    this.opentelemetry = new OpenTelemetry(this.client);
    this.payments = new Payments(this.client);
    this.pipelines = new Pipelines(this.client);
    this.platform = new Platform(this.client);
    this.projects = new Projects(this.client);
    this.speedInsights = new SpeedInsights(this.client);
    this.users = new Users(this.client);
    this.websocket = new WebSocket(this.client);
  }

  // Direct client access for advanced usage
  get rawClient() {
    return this.client;
  }
}
// Namespace classes
class APIKeys {
  constructor(private client: Client) { }

  activateApiKey = (options: Parameters<typeof sdk.activateApiKey>[0]) =>
    sdk.activateApiKey({ ...options, client: this.client });

  createApiKey = (options: Parameters<typeof sdk.createApiKey>[0]) =>
    sdk.createApiKey({ ...options, client: this.client });

  deactivateApiKey = (options: Parameters<typeof sdk.deactivateApiKey>[0]) =>
    sdk.deactivateApiKey({ ...options, client: this.client });

  deleteApiKey = (options: Parameters<typeof sdk.deleteApiKey>[0]) =>
    sdk.deleteApiKey({ ...options, client: this.client });

  getApiKey = (options: Parameters<typeof sdk.getApiKey>[0]) =>
    sdk.getApiKey({ ...options, client: this.client });

  getApiKeyPermissions = (options?: Parameters<typeof sdk.getApiKeyPermissions>[0]) =>
    sdk.getApiKeyPermissions({ ...options, client: this.client });

  listApiKeys = (options?: Parameters<typeof sdk.listApiKeys>[0]) =>
    sdk.listApiKeys({ ...options, client: this.client });

  updateApiKey = (options: Parameters<typeof sdk.updateApiKey>[0]) =>
    sdk.updateApiKey({ ...options, client: this.client });
}

class Analytics {
  constructor(private client: Client) { }

  enrichVisitor = (options: Parameters<typeof sdk.enrichVisitor>[0]) =>
    sdk.enrichVisitor({ ...options, client: this.client });

  getAnalyticsMetrics = (options: Parameters<typeof sdk.getAnalyticsMetrics>[0]) =>
    sdk.getAnalyticsMetrics({ ...options, client: this.client });

  getBrowsers = (options: Parameters<typeof sdk.getBrowsers>[0]) =>
    sdk.getBrowsers({ ...options, client: this.client });

  getEventsCount = (options: Parameters<typeof sdk.getEventsCount>[0]) =>
    sdk.getEventsCount({ ...options, client: this.client });

  getPathVisitors = (options: Parameters<typeof sdk.getPathVisitors>[0]) =>
    sdk.getPathVisitors({ ...options, client: this.client });

  getReferrers = (options: Parameters<typeof sdk.getReferrers>[0]) =>
    sdk.getReferrers({ ...options, client: this.client });

  getSessionDetails = (options: Parameters<typeof sdk.getSessionDetails>[0]) =>
    sdk.getSessionDetails({ ...options, client: this.client });

  getSessionEvents = (options: Parameters<typeof sdk.getSessionEvents>[0]) =>
    sdk.getSessionEvents({ ...options, client: this.client });

  getSessionLogs = (options: Parameters<typeof sdk.getSessionLogs>[0]) =>
    sdk.getSessionLogs({ ...options, client: this.client });

  getSessionMetrics = (options: Parameters<typeof sdk.getSessionMetrics>[0]) =>
    sdk.getSessionMetrics({ ...options, client: this.client });

  getStatusCodes = (options: Parameters<typeof sdk.getStatusCodes>[0]) =>
    sdk.getStatusCodes({ ...options, client: this.client });

  getViewsOverTime = (options: Parameters<typeof sdk.getViewsOverTime>[0]) =>
    sdk.getViewsOverTime({ ...options, client: this.client });

  getVisitorDetails = (options: Parameters<typeof sdk.getVisitorDetails>[0]) =>
    sdk.getVisitorDetails({ ...options, client: this.client });

  getVisitorLocations = (options: Parameters<typeof sdk.getVisitorLocations>[0]) =>
    sdk.getVisitorLocations({ ...options, client: this.client });

  getVisitors = (options: Parameters<typeof sdk.getVisitors>[0]) =>
    sdk.getVisitors({ ...options, client: this.client });

  getVisitorSessions = (options: Parameters<typeof sdk.getVisitorSessions>[0]) =>
    sdk.getVisitorSessions({ ...options, client: this.client });
}

class AuditLogs {
  constructor(private client: Client) { }

  getAuditLog = (options: Parameters<typeof sdk.getAuditLog>[0]) =>
    sdk.getAuditLog({ ...options, client: this.client });

  listAuditLogs = (options: Parameters<typeof sdk.listAuditLogs>[0]) =>
    sdk.listAuditLogs({ ...options, client: this.client });
}

class Authentication {
  constructor(private client: Client) { }

  authStatus = (options: Parameters<typeof sdk.authStatus>[0]) =>
    sdk.authStatus({ ...options, client: this.client });

  cliLogin = (options: Parameters<typeof sdk.cliLogin>[0]) =>
    sdk.cliLogin({ ...options, client: this.client });

  getCurrentUser = (options?: Parameters<typeof sdk.getCurrentUser>[0]) =>
    sdk.getCurrentUser({ ...options, client: this.client });

  githubAppCallback = (options: Parameters<typeof sdk.githubAppCallback>[0]) =>
    sdk.githubAppCallback({ ...options, client: this.client });

  githubCallback = (options: Parameters<typeof sdk.githubCallback>[0]) =>
    sdk.githubCallback({ ...options, client: this.client });

  initAuth = (options?: Parameters<typeof sdk.initAuth>[0]) =>
    sdk.initAuth({ ...options, client: this.client });

  login = (options?: Parameters<typeof sdk.login>[0]) =>
    sdk.login({ ...options, client: this.client });

  logout = (options?: Parameters<typeof sdk.logout>[0]) =>
    sdk.logout({ ...options, client: this.client });

  renewToken = (options: Parameters<typeof sdk.renewToken>[0]) =>
    sdk.renewToken({ ...options, client: this.client });

  verifyMfaChallenge = (options: Parameters<typeof sdk.verifyMfaChallenge>[0]) =>
    sdk.verifyMfaChallenge({ ...options, client: this.client });
}

class Backups {
  constructor(private client: Client) { }

  createBackupSchedule = (options: Parameters<typeof sdk.createBackupSchedule>[0]) =>
    sdk.createBackupSchedule({ ...options, client: this.client });

  createS3Source = (options: Parameters<typeof sdk.createS3Source>[0]) =>
    sdk.createS3Source({ ...options, client: this.client });

  deleteBackupSchedule = (options: Parameters<typeof sdk.deleteBackupSchedule>[0]) =>
    sdk.deleteBackupSchedule({ ...options, client: this.client });

  deleteS3Source = (options: Parameters<typeof sdk.deleteS3Source>[0]) =>
    sdk.deleteS3Source({ ...options, client: this.client });

  disableBackupSchedule = (options: Parameters<typeof sdk.disableBackupSchedule>[0]) =>
    sdk.disableBackupSchedule({ ...options, client: this.client });

  enableBackupSchedule = (options: Parameters<typeof sdk.enableBackupSchedule>[0]) =>
    sdk.enableBackupSchedule({ ...options, client: this.client });

  getBackup = (options: Parameters<typeof sdk.getBackup>[0]) =>
    sdk.getBackup({ ...options, client: this.client });

  getBackupSchedule = (options: Parameters<typeof sdk.getBackupSchedule>[0]) =>
    sdk.getBackupSchedule({ ...options, client: this.client });

  getS3Source = (options: Parameters<typeof sdk.getS3Source>[0]) =>
    sdk.getS3Source({ ...options, client: this.client });

  listBackupSchedules = (options?: Parameters<typeof sdk.listBackupSchedules>[0]) =>
    sdk.listBackupSchedules({ ...options, client: this.client });

  listBackupsForSchedule = (options: Parameters<typeof sdk.listBackupsForSchedule>[0]) =>
    sdk.listBackupsForSchedule({ ...options, client: this.client });

  listS3Sources = (options?: Parameters<typeof sdk.listS3Sources>[0]) =>
    sdk.listS3Sources({ ...options, client: this.client });

  listSourceBackups = (options: Parameters<typeof sdk.listSourceBackups>[0]) =>
    sdk.listSourceBackups({ ...options, client: this.client });

  runBackupForSource = (options: Parameters<typeof sdk.runBackupForSource>[0]) =>
    sdk.runBackupForSource({ ...options, client: this.client });

  updateS3Source = (options: Parameters<typeof sdk.updateS3Source>[0]) =>
    sdk.updateS3Source({ ...options, client: this.client });
}

class Crons {
  constructor(private client: Client) { }

  getCronById = (options: Parameters<typeof sdk.getCronById>[0]) =>
    sdk.getCronById({ ...options, client: this.client });

  getCronExecutions = (options: Parameters<typeof sdk.getCronExecutions>[0]) =>
    sdk.getCronExecutions({ ...options, client: this.client });

  getEnvironmentCrons = (options: Parameters<typeof sdk.getEnvironmentCrons>[0]) =>
    sdk.getEnvironmentCrons({ ...options, client: this.client });
}

class Deployments {
  constructor(private client: Client) { }

  getDeployment = (options: Parameters<typeof sdk.getDeployment>[0]) =>
    sdk.getDeployment({ ...options, client: this.client });

  getDeploymentMetricsHistogram = (options: Parameters<typeof sdk.getDeploymentMetricsHistogram>[0]) =>
    sdk.getDeploymentMetricsHistogram({ ...options, client: this.client });

  getDeploymentStageLogs = (options: Parameters<typeof sdk.getDeploymentStageLogs>[0]) =>
    sdk.getDeploymentStageLogs({ ...options, client: this.client });

  getDeploymentStages = (options: Parameters<typeof sdk.getDeploymentStages>[0]) =>
    sdk.getDeploymentStages({ ...options, client: this.client });

  getLastDeployment = (options: Parameters<typeof sdk.getLastDeployment>[0]) =>
    sdk.getLastDeployment({ ...options, client: this.client });

  tailDeploymentStageLogs = (options: Parameters<typeof sdk.tailDeploymentStageLogs>[0]) =>
    sdk.tailDeploymentStageLogs({ ...options, client: this.client });
}

class Develop {
  constructor(private client: Client) { }

  buildDevContainer = (options: Parameters<typeof sdk.buildDevContainer>[0]) =>
    sdk.buildDevContainer({ ...options, client: this.client });

  createBranch = (options: Parameters<typeof sdk.createBranch>[0]) =>
    sdk.createBranch({ ...options, client: this.client });

  createDevProject = (options: Parameters<typeof sdk.createDevProject>[0]) =>
    sdk.createDevProject({ ...options, client: this.client });

  createDirectory = (options: Parameters<typeof sdk.createDirectory>[0]) =>
    sdk.createDirectory({ ...options, client: this.client });

  createFile = (options: Parameters<typeof sdk.createFile>[0]) =>
    sdk.createFile({ ...options, client: this.client });

  deleteDevProject = (options: Parameters<typeof sdk.deleteDevProject>[0]) =>
    sdk.deleteDevProject({ ...options, client: this.client });

  deleteFileOrDirectory = (options: Parameters<typeof sdk.deleteFileOrDirectory>[0]) =>
    sdk.deleteFileOrDirectory({ ...options, client: this.client });

  devTerminalWs = (options: Parameters<typeof sdk.devTerminalWs>[0]) =>
    sdk.devTerminalWs({ ...options, client: this.client });

  getBranches = (options: Parameters<typeof sdk.getBranches>[0]) =>
    sdk.getBranches({ ...options, client: this.client });

  getDevProject = (options: Parameters<typeof sdk.getDevProject>[0]) =>
    sdk.getDevProject({ ...options, client: this.client });

  getDevProjects = (options?: Parameters<typeof sdk.getDevProjects>[0]) =>
    sdk.getDevProjects({ ...options, client: this.client });

  getGitLog = (options: Parameters<typeof sdk.getGitLog>[0]) =>
    sdk.getGitLog({ ...options, client: this.client });

  getGitStatus = (options: Parameters<typeof sdk.getGitStatus>[0]) =>
    sdk.getGitStatus({ ...options, client: this.client });

  gitAdd = (options: Parameters<typeof sdk.gitAdd>[0]) =>
    sdk.gitAdd({ ...options, client: this.client });

  gitCommit = (options: Parameters<typeof sdk.gitCommit>[0]) =>
    sdk.gitCommit({ ...options, client: this.client });

  gitPull = (options: Parameters<typeof sdk.gitPull>[0]) =>
    sdk.gitPull({ ...options, client: this.client });

  gitPush = (options: Parameters<typeof sdk.gitPush>[0]) =>
    sdk.gitPush({ ...options, client: this.client });

  gitRemove = (options: Parameters<typeof sdk.gitRemove>[0]) =>
    sdk.gitRemove({ ...options, client: this.client });

  gitUnstage = (options: Parameters<typeof sdk.gitUnstage>[0]) =>
    sdk.gitUnstage({ ...options, client: this.client });

  listDirectory = (options: Parameters<typeof sdk.listDirectory>[0]) =>
    sdk.listDirectory({ ...options, client: this.client });

  pullDevProject = (options: Parameters<typeof sdk.pullDevProject>[0]) =>
    sdk.pullDevProject({ ...options, client: this.client });

  readFile = (options: Parameters<typeof sdk.readFile>[0]) =>
    sdk.readFile({ ...options, client: this.client });

  startDevContainer = (options: Parameters<typeof sdk.startDevContainer>[0]) =>
    sdk.startDevContainer({ ...options, client: this.client });

  stopDevContainer = (options: Parameters<typeof sdk.stopDevContainer>[0]) =>
    sdk.stopDevContainer({ ...options, client: this.client });

  switchBranch = (options: Parameters<typeof sdk.switchBranch>[0]) =>
    sdk.switchBranch({ ...options, client: this.client });

  writeFile = (options: Parameters<typeof sdk.writeFile>[0]) =>
    sdk.writeFile({ ...options, client: this.client });
}

class Domains {
  constructor(private client: Client) { }

  checkDomainStatus = (options: Parameters<typeof sdk.checkDomainStatus>[0]) =>
    sdk.checkDomainStatus({ ...options, client: this.client });

  completeDnsChallenge = (options: Parameters<typeof sdk.completeDnsChallenge>[0]) =>
    sdk.completeDnsChallenge({ ...options, client: this.client });

  createDomain = (options: Parameters<typeof sdk.createDomain>[0]) =>
    sdk.createDomain({ ...options, client: this.client });

  deleteDomain = (options: Parameters<typeof sdk.deleteDomain>[0]) =>
    sdk.deleteDomain({ ...options, client: this.client });

  getDomainByHost = (options: Parameters<typeof sdk.getDomainByHost>[0]) =>
    sdk.getDomainByHost({ ...options, client: this.client });

  getDomainById = (options: Parameters<typeof sdk.getDomainById>[0]) =>
    sdk.getDomainById({ ...options, client: this.client });

  listDomains = (options?: Parameters<typeof sdk.listDomains>[0]) =>
    sdk.listDomains({ ...options, client: this.client });

  provisionDomain = (options: Parameters<typeof sdk.provisionDomain>[0]) =>
    sdk.provisionDomain({ ...options, client: this.client });

  renewDomain = (options: Parameters<typeof sdk.renewDomain>[0]) =>
    sdk.renewDomain({ ...options, client: this.client });
}

class Email {
  constructor(private client: Client) { }

  // Email Providers
  createProvider = (options: Parameters<typeof sdk.createEmailProvider2>[0]) =>
    sdk.createEmailProvider2({ ...options, client: this.client });

  listProviders = (options?: Parameters<typeof sdk.listEmailProviders>[0]) =>
    sdk.listEmailProviders({ ...options, client: this.client });

  getProvider = (options: Parameters<typeof sdk.getEmailProvider>[0]) =>
    sdk.getEmailProvider({ ...options, client: this.client });

  deleteProvider = (options: Parameters<typeof sdk.deleteEmailProvider>[0]) =>
    sdk.deleteEmailProvider({ ...options, client: this.client });

  // Email Domains
  createDomain = (options: Parameters<typeof sdk.createEmailDomain>[0]) =>
    sdk.createEmailDomain({ ...options, client: this.client });

  listDomains = (options?: Parameters<typeof sdk.listEmailDomains>[0]) =>
    sdk.listEmailDomains({ ...options, client: this.client });

  getDomain = (options: Parameters<typeof sdk.getEmailDomain>[0]) =>
    sdk.getEmailDomain({ ...options, client: this.client });

  deleteDomain = (options: Parameters<typeof sdk.deleteEmailDomain>[0]) =>
    sdk.deleteEmailDomain({ ...options, client: this.client });

  verifyDomain = (options: Parameters<typeof sdk.verifyEmailDomain>[0]) =>
    sdk.verifyEmailDomain({ ...options, client: this.client });

  // Emails
  send = (options: Parameters<typeof sdk.sendEmail>[0]) =>
    sdk.sendEmail({ ...options, client: this.client });

  list = (options?: Parameters<typeof sdk.listEmails>[0]) =>
    sdk.listEmails({ ...options, client: this.client });

  get = (options: Parameters<typeof sdk.getEmail>[0]) =>
    sdk.getEmail({ ...options, client: this.client });

  getStats = (options?: Parameters<typeof sdk.getEmailStats>[0]) =>
    sdk.getEmailStats({ ...options, client: this.client });
}

class ExternalServices {
  constructor(private client: Client) { }

  createService = (options: Parameters<typeof sdk.createService>[0]) =>
    sdk.createService({ ...options, client: this.client });

  deleteService = (options: Parameters<typeof sdk.deleteService>[0]) =>
    sdk.deleteService({ ...options, client: this.client });

  getProjectServiceEnvironmentVariables = (options: Parameters<typeof sdk.getProjectServiceEnvironmentVariables>[0]) =>
    sdk.getProjectServiceEnvironmentVariables({ ...options, client: this.client });

  getService = (options: Parameters<typeof sdk.getService>[0]) =>
    sdk.getService({ ...options, client: this.client });

  getServiceBySlug = (options: Parameters<typeof sdk.getServiceBySlug>[0]) =>
    sdk.getServiceBySlug({ ...options, client: this.client });

  getServiceEnvironmentVariable = (options: Parameters<typeof sdk.getServiceEnvironmentVariable>[0]) =>
    sdk.getServiceEnvironmentVariable({ ...options, client: this.client });

  getServiceEnvironmentVariables = (options: Parameters<typeof sdk.getServiceEnvironmentVariables>[0]) =>
    sdk.getServiceEnvironmentVariables({ ...options, client: this.client });

  getServiceTypeParameters = (options: Parameters<typeof sdk.getServiceTypeParameters>[0]) =>
    sdk.getServiceTypeParameters({ ...options, client: this.client });

  getServiceTypes = (options?: Parameters<typeof sdk.getServiceTypes>[0]) =>
    sdk.getServiceTypes({ ...options, client: this.client });

  linkServiceToProject = (options: Parameters<typeof sdk.linkServiceToProject>[0]) =>
    sdk.linkServiceToProject({ ...options, client: this.client });

  listProjectServices = (options: Parameters<typeof sdk.listProjectServices>[0]) =>
    sdk.listProjectServices({ ...options, client: this.client });

  listServiceProjects = (options: Parameters<typeof sdk.listServiceProjects>[0]) =>
    sdk.listServiceProjects({ ...options, client: this.client });

  listServices = (options?: Parameters<typeof sdk.listServices>[0]) =>
    sdk.listServices({ ...options, client: this.client });

  startService = (options: Parameters<typeof sdk.startService>[0]) =>
    sdk.startService({ ...options, client: this.client });

  stopService = (options: Parameters<typeof sdk.stopService>[0]) =>
    sdk.stopService({ ...options, client: this.client });

  unlinkServiceFromProject = (options: Parameters<typeof sdk.unlinkServiceFromProject>[0]) =>
    sdk.unlinkServiceFromProject({ ...options, client: this.client });

  updateService = (options: Parameters<typeof sdk.updateService>[0]) =>
    sdk.updateService({ ...options, client: this.client });
}

class FeatureFlags {
  constructor(private client: Client) { }

  getFeatureFlags = (options?: Parameters<typeof sdk.getFeatureFlags>[0]) =>
    sdk.getFeatureFlags({ ...options, client: this.client });

  updateAutomaticAnalytics = (options: Parameters<typeof sdk.updateAutomaticAnalytics>[0]) =>
    sdk.updateAutomaticAnalytics({ ...options, client: this.client });
}

class Files {
  constructor(private client: Client) { }

  getFile = (options: Parameters<typeof sdk.getFile>[0]) =>
    sdk.getFile({ ...options, client: this.client });
}

class Funnels {
  constructor(private client: Client) { }

  createFunnel = (options: Parameters<typeof sdk.createFunnel>[0]) =>
    sdk.createFunnel({ ...options, client: this.client });

  deleteFunnel = (options: Parameters<typeof sdk.deleteFunnel>[0]) =>
    sdk.deleteFunnel({ ...options, client: this.client });

  getFunnelMetrics = (options: Parameters<typeof sdk.getFunnelMetrics>[0]) =>
    sdk.getFunnelMetrics({ ...options, client: this.client });

  listFunnels = (options: Parameters<typeof sdk.listFunnels>[0]) =>
    sdk.listFunnels({ ...options, client: this.client });

  updateFunnel = (options: Parameters<typeof sdk.updateFunnel>[0]) =>
    sdk.updateFunnel({ ...options, client: this.client });
}

class Github {
  constructor(private client: Client) { }

  deleteGithubInstallation = (options: Parameters<typeof sdk.deleteGithubInstallation>[0]) =>
    sdk.deleteGithubInstallation({ ...options, client: this.client });

  getAllGithubApps = (options?: Parameters<typeof sdk.getAllGithubApps>[0]) =>
    sdk.getAllGithubApps({ ...options, client: this.client });

  getAllGithubInstallations = (options?: Parameters<typeof sdk.getAllGithubInstallations>[0]) =>
    sdk.getAllGithubInstallations({ ...options, client: this.client });

  getAllGithubRepos = (options: Parameters<typeof sdk.getAllGithubRepos>[0]) =>
    sdk.getAllGithubRepos({ ...options, client: this.client });

  getGithubRepoByOwnerName = (options: Parameters<typeof sdk.getGithubRepoByOwnerName>[0]) =>
    sdk.getGithubRepoByOwnerName({ ...options, client: this.client });

  getGithubRepoPreset = (options: Parameters<typeof sdk.getGithubRepoPreset>[0]) =>
    sdk.getGithubRepoPreset({ ...options, client: this.client });

  getRepoBranches = (options: Parameters<typeof sdk.getRepoBranches>[0]) =>
    sdk.getRepoBranches({ ...options, client: this.client });

  getRepoSources = (options?: Parameters<typeof sdk.getRepoSources>[0]) =>
    sdk.getRepoSources({ ...options, client: this.client });

  redirectToGithubInstall = (options?: Parameters<typeof sdk.redirectToGithubInstall>[0]) =>
    sdk.redirectToGithubInstall({ ...options, client: this.client });

  setupStatus = (options?: Parameters<typeof sdk.setupStatus>[0]) =>
    sdk.setupStatus({ ...options, client: this.client });

  syncGithubInstallation = (options: Parameters<typeof sdk.syncGithubInstallation>[0]) =>
    sdk.syncGithubInstallation({ ...options, client: this.client });

  githubWebhook = (options: Parameters<typeof sdk.githubWebhook>[0]) =>
    sdk.githubWebhook({ ...options, client: this.client });
}

class LoadBalancer {
  constructor(private client: Client) { }

  createRoute = (options: Parameters<typeof sdk.createRoute>[0]) =>
    sdk.createRoute({ ...options, client: this.client });

  deleteRoute = (options: Parameters<typeof sdk.deleteRoute>[0]) =>
    sdk.deleteRoute({ ...options, client: this.client });

  getRoute = (options: Parameters<typeof sdk.getRoute>[0]) =>
    sdk.getRoute({ ...options, client: this.client });

  listRoutes = (options?: Parameters<typeof sdk.listRoutes>[0]) =>
    sdk.listRoutes({ ...options, client: this.client });

  provisionLbCertificate = (options: Parameters<typeof sdk.provisionLbCertificate>[0]) =>
    sdk.provisionLbCertificate({ ...options, client: this.client });

  renewCertificate = (options: Parameters<typeof sdk.renewCertificate>[0]) =>
    sdk.renewCertificate({ ...options, client: this.client });

  updateRoute = (options: Parameters<typeof sdk.updateRoute>[0]) =>
    sdk.updateRoute({ ...options, client: this.client });
}

class Logs {
  constructor(private client: Client) { }

  getLogById = (options: Parameters<typeof sdk.getLogById>[0]) =>
    sdk.getLogById({ ...options, client: this.client });

  getLogs = (options: Parameters<typeof sdk.getLogs>[0]) =>
    sdk.getLogs({ ...options, client: this.client });

  getTodayErrorsCount = (options: Parameters<typeof sdk.getTodayErrorsCount>[0]) =>
    sdk.getTodayErrorsCount({ ...options, client: this.client });

  streamLogs = (options: Parameters<typeof sdk.streamLogs>[0]) =>
    sdk.streamLogs({ ...options, client: this.client });
}

class MCP {
  constructor(private client: Client) { }

  addClient = (options: Parameters<typeof sdk.addClient>[0]) =>
    sdk.addClient({ ...options, client: this.client });

  connectClient = (options: Parameters<typeof sdk.connectClient>[0]) =>
    sdk.connectClient({ ...options, client: this.client });

  listClients = (options?: Parameters<typeof sdk.listClients>[0]) =>
    sdk.listClients({ ...options, client: this.client });

  removeClient = (options: Parameters<typeof sdk.removeClient>[0]) =>
    sdk.removeClient({ ...options, client: this.client });
}

class Metrics {
  constructor(private client: Client) { }

  getAnalyticsScript = (options?: Parameters<typeof sdk.getAnalyticsScript>[0]) =>
    sdk.getAnalyticsScript({ ...options, client: this.client });

  getPerformanceScript = (options?: Parameters<typeof sdk.getPerformanceScript>[0]) =>
    sdk.getPerformanceScript({ ...options, client: this.client });

  recordEventMetrics = (options: Parameters<typeof sdk.recordEventMetrics>[0]) =>
    sdk.recordEventMetrics({ ...options, client: this.client });

  recordSpeedMetrics = (options: Parameters<typeof sdk.recordSpeedMetrics>[0]) =>
    sdk.recordSpeedMetrics({ ...options, client: this.client });

  updateSpeedMetrics = (options: Parameters<typeof sdk.updateSpeedMetrics>[0]) =>
    sdk.updateSpeedMetrics({ ...options, client: this.client });
}

class Notifications {
  constructor(private client: Client) { }

  createEmailProvider = (options: Parameters<typeof sdk.createEmailProvider>[0]) =>
    sdk.createEmailProvider({ ...options, client: this.client });

  createProvider = (options: Parameters<typeof sdk.createProvider>[0]) =>
    sdk.createProvider({ ...options, client: this.client });

  createSlackProvider = (options: Parameters<typeof sdk.createSlackProvider>[0]) =>
    sdk.createSlackProvider({ ...options, client: this.client });

  deletePreferences = (options?: Parameters<typeof sdk.deletePreferences>[0]) =>
    sdk.deletePreferences({ ...options, client: this.client });

  deleteProvider = (options: Parameters<typeof sdk.deleteProvider>[0]) =>
    sdk.deleteProvider({ ...options, client: this.client });

  getPreferences = (options?: Parameters<typeof sdk.getPreferences>[0]) =>
    sdk.getPreferences({ ...options, client: this.client });

  getProvider = (options: Parameters<typeof sdk.getProvider>[0]) =>
    sdk.getProvider({ ...options, client: this.client });

  listProviders = (options?: Parameters<typeof sdk.listProviders>[0]) =>
    sdk.listProviders({ ...options, client: this.client });

  testProvider = (options: Parameters<typeof sdk.testProvider>[0]) =>
    sdk.testProvider({ ...options, client: this.client });

  updateEmailProvider = (options: Parameters<typeof sdk.updateEmailProvider>[0]) =>
    sdk.updateEmailProvider({ ...options, client: this.client });

  updatePreferences = (options: Parameters<typeof sdk.updatePreferences>[0]) =>
    sdk.updatePreferences({ ...options, client: this.client });

  updateProvider = (options: Parameters<typeof sdk.updateProvider>[0]) =>
    sdk.updateProvider({ ...options, client: this.client });

  updateSlackProvider = (options: Parameters<typeof sdk.updateSlackProvider>[0]) =>
    sdk.updateSlackProvider({ ...options, client: this.client });
}

class OpenTelemetry {
  constructor(private client: Client) { }

  getOpentelemetryLogs = (options: Parameters<typeof sdk.getOpentelemetryLogs>[0]) =>
    sdk.getOpentelemetryLogs({ ...options, client: this.client });

  getOpentelemetryTraces = (options: Parameters<typeof sdk.getOpentelemetryTraces>[0]) =>
    sdk.getOpentelemetryTraces({ ...options, client: this.client });

  getTraceDetails = (options: Parameters<typeof sdk.getTraceDetails>[0]) =>
    sdk.getTraceDetails({ ...options, client: this.client });

  getTracePercentiles = (options: Parameters<typeof sdk.getTracePercentiles>[0]) =>
    sdk.getTracePercentiles({ ...options, client: this.client });

  ingestLogs = (options: Parameters<typeof sdk.ingestLogs>[0]) =>
    sdk.ingestLogs({ ...options, client: this.client });

  ingestTraces = (options: Parameters<typeof sdk.ingestTraces>[0]) =>
    sdk.ingestTraces({ ...options, client: this.client });
}

class Payments {
  constructor(private client: Client) { }

  createPaymentProvider = (options: Parameters<typeof sdk.createPaymentProvider>[0]) =>
    sdk.createPaymentProvider({ ...options, client: this.client });

  createProduct = (options: Parameters<typeof sdk.createProduct>[0]) =>
    sdk.createProduct({ ...options, client: this.client });

  createWebhook = (options: Parameters<typeof sdk.createWebhook>[0]) =>
    sdk.createWebhook({ ...options, client: this.client });

  deletePaymentProvider = (options: Parameters<typeof sdk.deletePaymentProvider>[0]) =>
    sdk.deletePaymentProvider({ ...options, client: this.client });

  deleteProduct = (options: Parameters<typeof sdk.deleteProduct>[0]) =>
    sdk.deleteProduct({ ...options, client: this.client });

  deleteWebhook = (options: Parameters<typeof sdk.deleteWebhook>[0]) =>
    sdk.deleteWebhook({ ...options, client: this.client });

  getPaymentEnvironmentVariables = (options: Parameters<typeof sdk.getPaymentEnvironmentVariables>[0]) =>
    sdk.getPaymentEnvironmentVariables({ ...options, client: this.client });

  getPaymentMetrics = (options: Parameters<typeof sdk.getPaymentMetrics>[0]) =>
    sdk.getPaymentMetrics({ ...options, client: this.client });

  getPaymentProvider = (options: Parameters<typeof sdk.getPaymentProvider>[0]) =>
    sdk.getPaymentProvider({ ...options, client: this.client });

  getPaymentSettings = (options: Parameters<typeof sdk.getPaymentSettings>[0]) =>
    sdk.getPaymentSettings({ ...options, client: this.client });

  getProduct = (options: Parameters<typeof sdk.getProduct>[0]) =>
    sdk.getProduct({ ...options, client: this.client });

  getProducts = (options: Parameters<typeof sdk.getProducts>[0]) =>
    sdk.getProducts({ ...options, client: this.client });

  getProjectPaymentProvider = (options: Parameters<typeof sdk.getProjectPaymentProvider>[0]) =>
    sdk.getProjectPaymentProvider({ ...options, client: this.client });

  getTodayRevenue = (options: Parameters<typeof sdk.getTodayRevenue>[0]) =>
    sdk.getTodayRevenue({ ...options, client: this.client });

  getTotalRevenue = (options?: Parameters<typeof sdk.getTotalRevenue>[0]) =>
    sdk.getTotalRevenue({ ...options, client: this.client });

  getWebhook = (options: Parameters<typeof sdk.getWebhook>[0]) =>
    sdk.getWebhook({ ...options, client: this.client });

  getWebhookLog = (options: Parameters<typeof sdk.getWebhookLog>[0]) =>
    sdk.getWebhookLog({ ...options, client: this.client });

  getWebhookLogs = (options: Parameters<typeof sdk.getWebhookLogs>[0]) =>
    sdk.getWebhookLogs({ ...options, client: this.client });

  listPaymentProviders = (options?: Parameters<typeof sdk.listPaymentProviders>[0]) =>
    sdk.listPaymentProviders({ ...options, client: this.client });

  listWebhooks = (options: Parameters<typeof sdk.listWebhooks>[0]) =>
    sdk.listWebhooks({ ...options, client: this.client });

  retryWebhook = (options: Parameters<typeof sdk.retryWebhook>[0]) =>
    sdk.retryWebhook({ ...options, client: this.client });

  setDefaultPaymentProvider = (options: Parameters<typeof sdk.setDefaultPaymentProvider>[0]) =>
    sdk.setDefaultPaymentProvider({ ...options, client: this.client });

  setEnvironmentMode = (options: Parameters<typeof sdk.setEnvironmentMode>[0]) =>
    sdk.setEnvironmentMode({ ...options, client: this.client });

  setProjectPaymentProvider = (options: Parameters<typeof sdk.setProjectPaymentProvider>[0]) =>
    sdk.setProjectPaymentProvider({ ...options, client: this.client });

  testPaymentProvider = (options: Parameters<typeof sdk.testPaymentProvider>[0]) =>
    sdk.testPaymentProvider({ ...options, client: this.client });

  updatePaymentProvider = (options: Parameters<typeof sdk.updatePaymentProvider>[0]) =>
    sdk.updatePaymentProvider({ ...options, client: this.client });

  updateProduct = (options: Parameters<typeof sdk.updateProduct>[0]) =>
    sdk.updateProduct({ ...options, client: this.client });

  updateWebhook = (options: Parameters<typeof sdk.updateWebhook>[0]) =>
    sdk.updateWebhook({ ...options, client: this.client });
}

class Pipelines {
  constructor(private client: Client) { }

  getPipelineLogs = (options: Parameters<typeof sdk.getPipelineLogs>[0]) =>
    sdk.getPipelineLogs({ ...options, client: this.client });

  getProjectPipelines = (options: Parameters<typeof sdk.getProjectPipelines>[0]) =>
    sdk.getProjectPipelines({ ...options, client: this.client });
}

class Platform {
  constructor(private client: Client) { }

  getPlatformInfo = (options?: Parameters<typeof sdk.getPlatformInfo>[0]) =>
    sdk.getPlatformInfo({ ...options, client: this.client });
}

class Projects {
  constructor(private client: Client) { }

  addEnvironmentDomain = (options: Parameters<typeof sdk.addEnvironmentDomain>[0]) =>
    sdk.addEnvironmentDomain({ ...options, client: this.client });

  checkCustomDomainConfiguration = (options: Parameters<typeof sdk.checkCustomDomainConfiguration>[0]) =>
    sdk.checkCustomDomainConfiguration({ ...options, client: this.client });

  createCustomDomain = (options: Parameters<typeof sdk.createCustomDomain>[0]) =>
    sdk.createCustomDomain({ ...options, client: this.client });

  createEnvironment = (options: Parameters<typeof sdk.createEnvironment>[0]) =>
    sdk.createEnvironment({ ...options, client: this.client });

  createEnvironmentVariable = (options: Parameters<typeof sdk.createEnvironmentVariable>[0]) =>
    sdk.createEnvironmentVariable({ ...options, client: this.client });

  createProject = (options: Parameters<typeof sdk.createProject>[0]) =>
    sdk.createProject({ ...options, client: this.client });

  createProjectFromTemplate = (options: Parameters<typeof sdk.createProjectFromTemplate>[0]) =>
    sdk.createProjectFromTemplate({ ...options, client: this.client });

  deleteCustomDomain = (options: Parameters<typeof sdk.deleteCustomDomain>[0]) =>
    sdk.deleteCustomDomain({ ...options, client: this.client });

  deleteEnvironmentDomain = (options: Parameters<typeof sdk.deleteEnvironmentDomain>[0]) =>
    sdk.deleteEnvironmentDomain({ ...options, client: this.client });

  deleteEnvironmentVariable = (options: Parameters<typeof sdk.deleteEnvironmentVariable>[0]) =>
    sdk.deleteEnvironmentVariable({ ...options, client: this.client });

  deleteProject = (options: Parameters<typeof sdk.deleteProject>[0]) =>
    sdk.deleteProject({ ...options, client: this.client });

  getContainerLogs = (options: Parameters<typeof sdk.getContainerLogs>[0]) =>
    sdk.getContainerLogs({ ...options, client: this.client });

  getCustomDomains = (options: Parameters<typeof sdk.getCustomDomains>[0]) =>
    sdk.getCustomDomains({ ...options, client: this.client });

  getEnvironment = (options: Parameters<typeof sdk.getEnvironment>[0]) =>
    sdk.getEnvironment({ ...options, client: this.client });

  getEnvironmentDomains = (options: Parameters<typeof sdk.getEnvironmentDomains>[0]) =>
    sdk.getEnvironmentDomains({ ...options, client: this.client });

  getEnvironments = (options: Parameters<typeof sdk.getEnvironments>[0]) =>
    sdk.getEnvironments({ ...options, client: this.client });

  getEnvironmentVariables = (options: Parameters<typeof sdk.getEnvironmentVariables>[0]) =>
    sdk.getEnvironmentVariables({ ...options, client: this.client });

  getEnvironmentVariableValue = (options: Parameters<typeof sdk.getEnvironmentVariableValue>[0]) =>
    sdk.getEnvironmentVariableValue({ ...options, client: this.client });

  getHourlyVisitorStats = (options: Parameters<typeof sdk.getHourlyVisitorStats>[0]) =>
    sdk.getHourlyVisitorStats({ ...options, client: this.client });

  getProject = (options: Parameters<typeof sdk.getProject>[0]) =>
    sdk.getProject({ ...options, client: this.client });

  getProjectBySlug = (options: Parameters<typeof sdk.getProjectBySlug>[0]) =>
    sdk.getProjectBySlug({ ...options, client: this.client });

  getProjectDeployments = (options: Parameters<typeof sdk.getProjectDeployments>[0]) =>
    sdk.getProjectDeployments({ ...options, client: this.client });

  getProjectErrorStats = (options: Parameters<typeof sdk.getProjectErrorStats>[0]) =>
    sdk.getProjectErrorStats({ ...options, client: this.client });

  getProjectFavicon = (options: Parameters<typeof sdk.getProjectFavicon>[0]) =>
    sdk.getProjectFavicon({ ...options, client: this.client });

  getProjectRevenueStats = (options: Parameters<typeof sdk.getProjectRevenueStats>[0]) =>
    sdk.getProjectRevenueStats({ ...options, client: this.client });

  getProjects = (options?: Parameters<typeof sdk.getProjects>[0]) =>
    sdk.getProjects({ ...options, client: this.client });

  getProjectStats = (options?: Parameters<typeof sdk.getProjectStats>[0]) =>
    sdk.getProjectStats({ ...options, client: this.client });

  getProjectVisitorStats = (options: Parameters<typeof sdk.getProjectVisitorStats>[0]) =>
    sdk.getProjectVisitorStats({ ...options, client: this.client });

  getTemplateByName = (options: Parameters<typeof sdk.getTemplateByName>[0]) =>
    sdk.getTemplateByName({ ...options, client: this.client });

  getTemplatePreview = (options: Parameters<typeof sdk.getTemplatePreview>[0]) =>
    sdk.getTemplatePreview({ ...options, client: this.client });

  getTemplates = (options?: Parameters<typeof sdk.getTemplates>[0]) =>
    sdk.getTemplates({ ...options, client: this.client });

  getTotalVisitorStats = (options?: Parameters<typeof sdk.getTotalVisitorStats>[0]) =>
    sdk.getTotalVisitorStats({ ...options, client: this.client });

  manualDockerDeployment = (options: Parameters<typeof sdk.manualDockerDeployment>[0]) =>
    sdk.manualDockerDeployment({ ...options, client: this.client });

  manualStaticDeployment = (options: Parameters<typeof sdk.manualStaticDeployment>[0]) =>
    sdk.manualStaticDeployment({ ...options, client: this.client });

  pauseDeployment = (options: Parameters<typeof sdk.pauseDeployment>[0]) =>
    sdk.pauseDeployment({ ...options, client: this.client });

  provisionCertificate = (options: Parameters<typeof sdk.provisionCertificate>[0]) =>
    sdk.provisionCertificate({ ...options, client: this.client });

  resumeDeployment = (options: Parameters<typeof sdk.resumeDeployment>[0]) =>
    sdk.resumeDeployment({ ...options, client: this.client });

  rollbackToDeployment = (options: Parameters<typeof sdk.rollbackToDeployment>[0]) =>
    sdk.rollbackToDeployment({ ...options, client: this.client });

  teardownDeployment = (options: Parameters<typeof sdk.teardownDeployment>[0]) =>
    sdk.teardownDeployment({ ...options, client: this.client });

  teardownEnvironment = (options: Parameters<typeof sdk.teardownEnvironment>[0]) =>
    sdk.teardownEnvironment({ ...options, client: this.client });

  triggerProjectPipeline = (options: Parameters<typeof sdk.triggerProjectPipeline>[0]) =>
    sdk.triggerProjectPipeline({ ...options, client: this.client });

  updateAutomaticDeploy = (options: Parameters<typeof sdk.updateAutomaticDeploy>[0]) =>
    sdk.updateAutomaticDeploy({ ...options, client: this.client });

  updateCustomDomain = (options: Parameters<typeof sdk.updateCustomDomain>[0]) =>
    sdk.updateCustomDomain({ ...options, client: this.client });

  updateDeploymentSettings = (options: Parameters<typeof sdk.updateDeploymentSettings>[0]) =>
    sdk.updateDeploymentSettings({ ...options, client: this.client });

  updateEnvironmentSettings = (options: Parameters<typeof sdk.updateEnvironmentSettings>[0]) =>
    sdk.updateEnvironmentSettings({ ...options, client: this.client });

  updateEnvironmentVariable = (options: Parameters<typeof sdk.updateEnvironmentVariable>[0]) =>
    sdk.updateEnvironmentVariable({ ...options, client: this.client });

  updateGithubRepo = (options: Parameters<typeof sdk.updateGithubRepo>[0]) =>
    sdk.updateGithubRepo({ ...options, client: this.client });

  updateProject = (options: Parameters<typeof sdk.updateProject>[0]) =>
    sdk.updateProject({ ...options, client: this.client });

  updateProjectSettings = (options: Parameters<typeof sdk.updateProjectSettings>[0]) =>
    sdk.updateProjectSettings({ ...options, client: this.client });
}

class SpeedInsights {
  constructor(private client: Client) { }

  getSpeedMetricsOverTime = (options: Parameters<typeof sdk.getSpeedMetricsOverTime>[0]) =>
    sdk.getSpeedMetricsOverTime({ ...options, client: this.client });

  getSpeedPerformanceMetrics = (options: Parameters<typeof sdk.getSpeedPerformanceMetrics>[0]) =>
    sdk.getSpeedPerformanceMetrics({ ...options, client: this.client });
}

class Users {
  constructor(private client: Client) { }

  assignRole = (options: Parameters<typeof sdk.assignRole>[0]) =>
    sdk.assignRole({ ...options, client: this.client });

  createUser = (options: Parameters<typeof sdk.createUser>[0]) =>
    sdk.createUser({ ...options, client: this.client });

  deleteUser = (options: Parameters<typeof sdk.deleteUser>[0]) =>
    sdk.deleteUser({ ...options, client: this.client });

  disableMfa = (options: Parameters<typeof sdk.disableMfa>[0]) =>
    sdk.disableMfa({ ...options, client: this.client });

  listUsers = (options: Parameters<typeof sdk.listUsers>[0]) =>
    sdk.listUsers({ ...options, client: this.client });

  removeRole = (options: Parameters<typeof sdk.removeRole>[0]) =>
    sdk.removeRole({ ...options, client: this.client });

  restoreUser = (options: Parameters<typeof sdk.restoreUser>[0]) =>
    sdk.restoreUser({ ...options, client: this.client });

  setupMfa = (options?: Parameters<typeof sdk.setupMfa>[0]) =>
    sdk.setupMfa({ ...options, client: this.client });

  updateSelf = (options: Parameters<typeof sdk.updateSelf>[0]) =>
    sdk.updateSelf({ ...options, client: this.client });

  updateUser = (options: Parameters<typeof sdk.updateUser>[0]) =>
    sdk.updateUser({ ...options, client: this.client });

  verifyAndEnableMfa = (options: Parameters<typeof sdk.verifyAndEnableMfa>[0]) =>
    sdk.verifyAndEnableMfa({ ...options, client: this.client });
}

class WebSocket {
  constructor(private client: Client) { }

  wsHandler = (options: Parameters<typeof sdk.wsHandler>[0]) =>
    sdk.wsHandler({ ...options, client: this.client });
}


export default TempsClient;
