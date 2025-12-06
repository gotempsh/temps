import { createClient, createConfig } from './client/client';
import type { Client } from './client/client';
import * as sdk from './client/sdk.gen';

export * from './client/types.gen';
export * from './client/sdk.gen';
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
  dns: DNS;
  domains: Domains;
  email: Email;
  externalServices: ExternalServices;
  files: Files;
  funnels: Funnels;
  git: Git;
  loadBalancer: LoadBalancer;
  monitoring: Monitoring;
  notifications: Notifications;
  performance: Performance;
  platform: Platform;
  projects: Projects;
  proxyLogs: ProxyLogs;
  repositories: Repositories;
  sessionReplay: SessionReplay;
  settings: Settings;
  users: Users;

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
    this.dns = new DNS(this.client);
    this.domains = new Domains(this.client);
    this.email = new Email(this.client);
    this.externalServices = new ExternalServices(this.client);
    this.files = new Files(this.client);
    this.funnels = new Funnels(this.client);
    this.git = new Git(this.client);
    this.loadBalancer = new LoadBalancer(this.client);
    this.monitoring = new Monitoring(this.client);
    this.notifications = new Notifications(this.client);
    this.performance = new Performance(this.client);
    this.platform = new Platform(this.client);
    this.projects = new Projects(this.client);
    this.proxyLogs = new ProxyLogs(this.client);
    this.repositories = new Repositories(this.client);
    this.sessionReplay = new SessionReplay(this.client);
    this.settings = new Settings(this.client);
    this.users = new Users(this.client);
  }

  // Direct client access for advanced usage
  get rawClient() {
    return this.client;
  }
}

// Namespace classes
class APIKeys {
  constructor(private client: Client) { }

  activate = (options: Parameters<typeof sdk.activateApiKey>[0]) =>
    sdk.activateApiKey({ ...options, client: this.client });

  create = (options: Parameters<typeof sdk.createApiKey>[0]) =>
    sdk.createApiKey({ ...options, client: this.client });

  deactivate = (options: Parameters<typeof sdk.deactivateApiKey>[0]) =>
    sdk.deactivateApiKey({ ...options, client: this.client });

  delete = (options: Parameters<typeof sdk.deleteApiKey>[0]) =>
    sdk.deleteApiKey({ ...options, client: this.client });

  get = (options: Parameters<typeof sdk.getApiKey>[0]) =>
    sdk.getApiKey({ ...options, client: this.client });

  getPermissions = (options?: Parameters<typeof sdk.getApiKeyPermissions>[0]) =>
    sdk.getApiKeyPermissions({ ...options, client: this.client });

  list = (options?: Parameters<typeof sdk.listApiKeys>[0]) =>
    sdk.listApiKeys({ ...options, client: this.client });

  update = (options: Parameters<typeof sdk.updateApiKey>[0]) =>
    sdk.updateApiKey({ ...options, client: this.client });
}

class Analytics {
  constructor(private client: Client) { }

  enrichVisitor = (options: Parameters<typeof sdk.enrichVisitor>[0]) =>
    sdk.enrichVisitor({ ...options, client: this.client });

  getActiveVisitors = (options: Parameters<typeof sdk.getActiveVisitors>[0]) =>
    sdk.getActiveVisitors({ ...options, client: this.client });

  getEventsCount = (options: Parameters<typeof sdk.getEventsCount>[0]) =>
    sdk.getEventsCount({ ...options, client: this.client });

  getGeneralStats = (options: Parameters<typeof sdk.getGeneralStats>[0]) =>
    sdk.getGeneralStats({ ...options, client: this.client });

  getLiveVisitorsList = (options: Parameters<typeof sdk.getLiveVisitorsList>[0]) =>
    sdk.getLiveVisitorsList({ ...options, client: this.client });

  getPageHourlySessions = (options: Parameters<typeof sdk.getPageHourlySessions>[0]) =>
    sdk.getPageHourlySessions({ ...options, client: this.client });

  getPagePaths = (options: Parameters<typeof sdk.getPagePaths>[0]) =>
    sdk.getPagePaths({ ...options, client: this.client });

  getSessionDetails = (options: Parameters<typeof sdk.getSessionDetails>[0]) =>
    sdk.getSessionDetails({ ...options, client: this.client });

  getSessionEvents = (options: Parameters<typeof sdk.getSessionEvents>[0]) =>
    sdk.getSessionEvents({ ...options, client: this.client });

  getSessionLogs = (options: Parameters<typeof sdk.getSessionLogs>[0]) =>
    sdk.getSessionLogs({ ...options, client: this.client });

  getVisitorByGuid = (options: Parameters<typeof sdk.getVisitorByGuid>[0]) =>
    sdk.getVisitorByGuid({ ...options, client: this.client });

  getVisitorById = (options: Parameters<typeof sdk.getVisitorById>[0]) =>
    sdk.getVisitorById({ ...options, client: this.client });

  getVisitorDetails = (options: Parameters<typeof sdk.getVisitorDetails>[0]) =>
    sdk.getVisitorDetails({ ...options, client: this.client });

  getVisitorInfo = (options: Parameters<typeof sdk.getVisitorInfo>[0]) =>
    sdk.getVisitorInfo({ ...options, client: this.client });

  getVisitors = (options: Parameters<typeof sdk.getVisitors>[0]) =>
    sdk.getVisitors({ ...options, client: this.client });

  getVisitorSessions = (options: Parameters<typeof sdk.getVisitorSessions>[0]) =>
    sdk.getVisitorSessions({ ...options, client: this.client });

  getVisitorStats = (options: Parameters<typeof sdk.getVisitorStats>[0]) =>
    sdk.getVisitorStats({ ...options, client: this.client });

  recordEvent = (options: Parameters<typeof sdk.recordEventMetrics>[0]) =>
    sdk.recordEventMetrics({ ...options, client: this.client });
}

class AuditLogs {
  constructor(private client: Client) { }

  get = (options: Parameters<typeof sdk.getAuditLog>[0]) =>
    sdk.getAuditLog({ ...options, client: this.client });

  list = (options: Parameters<typeof sdk.listAuditLogs>[0]) =>
    sdk.listAuditLogs({ ...options, client: this.client });
}

class Authentication {
  constructor(private client: Client) { }

  emailStatus = (options?: Parameters<typeof sdk.emailStatus>[0]) =>
    sdk.emailStatus({ ...options, client: this.client });

  getCurrentUser = (options?: Parameters<typeof sdk.getCurrentUser>[0]) =>
    sdk.getCurrentUser({ ...options, client: this.client });

  login = (options: Parameters<typeof sdk.login>[0]) =>
    sdk.login({ ...options, client: this.client });

  logout = (options?: Parameters<typeof sdk.logout>[0]) =>
    sdk.logout({ ...options, client: this.client });

  requestMagicLink = (options: Parameters<typeof sdk.requestMagicLink>[0]) =>
    sdk.requestMagicLink({ ...options, client: this.client });

  requestPasswordReset = (options: Parameters<typeof sdk.requestPasswordReset>[0]) =>
    sdk.requestPasswordReset({ ...options, client: this.client });

  resetPassword = (options: Parameters<typeof sdk.resetPassword>[0]) =>
    sdk.resetPassword({ ...options, client: this.client });

  verifyEmail = (options: Parameters<typeof sdk.verifyEmail>[0]) =>
    sdk.verifyEmail({ ...options, client: this.client });

  verifyMagicLink = (options: Parameters<typeof sdk.verifyMagicLink>[0]) =>
    sdk.verifyMagicLink({ ...options, client: this.client });

  verifyMfaChallenge = (options: Parameters<typeof sdk.verifyMfaChallenge>[0]) =>
    sdk.verifyMfaChallenge({ ...options, client: this.client });
}

class Backups {
  constructor(private client: Client) { }

  createSchedule = (options: Parameters<typeof sdk.createBackupSchedule>[0]) =>
    sdk.createBackupSchedule({ ...options, client: this.client });

  createS3Source = (options: Parameters<typeof sdk.createS3Source>[0]) =>
    sdk.createS3Source({ ...options, client: this.client });

  deleteSchedule = (options: Parameters<typeof sdk.deleteBackupSchedule>[0]) =>
    sdk.deleteBackupSchedule({ ...options, client: this.client });

  deleteS3Source = (options: Parameters<typeof sdk.deleteS3Source>[0]) =>
    sdk.deleteS3Source({ ...options, client: this.client });

  disableSchedule = (options: Parameters<typeof sdk.disableBackupSchedule>[0]) =>
    sdk.disableBackupSchedule({ ...options, client: this.client });

  enableSchedule = (options: Parameters<typeof sdk.enableBackupSchedule>[0]) =>
    sdk.enableBackupSchedule({ ...options, client: this.client });

  get = (options: Parameters<typeof sdk.getBackup>[0]) =>
    sdk.getBackup({ ...options, client: this.client });

  getSchedule = (options: Parameters<typeof sdk.getBackupSchedule>[0]) =>
    sdk.getBackupSchedule({ ...options, client: this.client });

  getS3Source = (options: Parameters<typeof sdk.getS3Source>[0]) =>
    sdk.getS3Source({ ...options, client: this.client });

  listSchedules = (options?: Parameters<typeof sdk.listBackupSchedules>[0]) =>
    sdk.listBackupSchedules({ ...options, client: this.client });

  listBackupsForSchedule = (options: Parameters<typeof sdk.listBackupsForSchedule>[0]) =>
    sdk.listBackupsForSchedule({ ...options, client: this.client });

  listS3Sources = (options?: Parameters<typeof sdk.listS3Sources>[0]) =>
    sdk.listS3Sources({ ...options, client: this.client });

  listSourceBackups = (options: Parameters<typeof sdk.listSourceBackups>[0]) =>
    sdk.listSourceBackups({ ...options, client: this.client });

  runBackupForSource = (options: Parameters<typeof sdk.runBackupForSource>[0]) =>
    sdk.runBackupForSource({ ...options, client: this.client });

  runExternalServiceBackup = (options: Parameters<typeof sdk.runExternalServiceBackup>[0]) =>
    sdk.runExternalServiceBackup({ ...options, client: this.client });

  updateS3Source = (options: Parameters<typeof sdk.updateS3Source>[0]) =>
    sdk.updateS3Source({ ...options, client: this.client });
}

class Crons {
  constructor(private client: Client) { }

  get = (options: Parameters<typeof sdk.getCronById>[0]) =>
    sdk.getCronById({ ...options, client: this.client });

  getExecutions = (options: Parameters<typeof sdk.getCronExecutions>[0]) =>
    sdk.getCronExecutions({ ...options, client: this.client });

  listForEnvironment = (options: Parameters<typeof sdk.getEnvironmentCrons>[0]) =>
    sdk.getEnvironmentCrons({ ...options, client: this.client });
}

class Deployments {
  constructor(private client: Client) { }

  cancel = (options: Parameters<typeof sdk.cancelDeployment>[0]) =>
    sdk.cancelDeployment({ ...options, client: this.client });

  get = (options: Parameters<typeof sdk.getDeployment>[0]) =>
    sdk.getDeployment({ ...options, client: this.client });

  getJobs = (options: Parameters<typeof sdk.getDeploymentJobs>[0]) =>
    sdk.getDeploymentJobs({ ...options, client: this.client });

  getJobLogs = (options: Parameters<typeof sdk.getDeploymentJobLogs>[0]) =>
    sdk.getDeploymentJobLogs({ ...options, client: this.client });

  getLast = (options: Parameters<typeof sdk.getLastDeployment>[0]) =>
    sdk.getLastDeployment({ ...options, client: this.client });

  getOperations = (options: Parameters<typeof sdk.getDeploymentOperations>[0]) =>
    sdk.getDeploymentOperations({ ...options, client: this.client });

  getOperationStatus = (options: Parameters<typeof sdk.getDeploymentOperationStatus>[0]) =>
    sdk.getDeploymentOperationStatus({ ...options, client: this.client });

  executeOperation = (options: Parameters<typeof sdk.executeDeploymentOperation>[0]) =>
    sdk.executeDeploymentOperation({ ...options, client: this.client });

  pause = (options: Parameters<typeof sdk.pauseDeployment>[0]) =>
    sdk.pauseDeployment({ ...options, client: this.client });

  resume = (options: Parameters<typeof sdk.resumeDeployment>[0]) =>
    sdk.resumeDeployment({ ...options, client: this.client });

  rollback = (options: Parameters<typeof sdk.rollbackToDeployment>[0]) =>
    sdk.rollbackToDeployment({ ...options, client: this.client });

  teardown = (options: Parameters<typeof sdk.teardownDeployment>[0]) =>
    sdk.teardownDeployment({ ...options, client: this.client });

  tailJobLogs = (options: Parameters<typeof sdk.tailDeploymentJobLogs>[0]) =>
    sdk.tailDeploymentJobLogs({ ...options, client: this.client });
}

class DNS {
  constructor(private client: Client) { }

  // DNS Providers
  listProviders = (options?: Parameters<typeof sdk.listProviders>[0]) =>
    sdk.listProviders({ ...options, client: this.client });

  createProvider = (options: Parameters<typeof sdk.createProvider>[0]) =>
    sdk.createProvider({ ...options, client: this.client });

  deleteProvider = (options: Parameters<typeof sdk.deleteProvider>[0]) =>
    sdk.deleteProvider({ ...options, client: this.client });

  getProvider = (options: Parameters<typeof sdk.getProvider>[0]) =>
    sdk.getProvider({ ...options, client: this.client });

  updateProvider = (options: Parameters<typeof sdk.updateProvider>[0]) =>
    sdk.updateProvider({ ...options, client: this.client });

  testProviderConnection = (options: Parameters<typeof sdk.testProviderConnection>[0]) =>
    sdk.testProviderConnection({ ...options, client: this.client });

  listProviderZones = (options: Parameters<typeof sdk.listProviderZones>[0]) =>
    sdk.listProviderZones({ ...options, client: this.client });

  // Managed Domains
  listManagedDomains = (options: Parameters<typeof sdk.listManagedDomains>[0]) =>
    sdk.listManagedDomains({ ...options, client: this.client });

  addManagedDomain = (options: Parameters<typeof sdk.addManagedDomain>[0]) =>
    sdk.addManagedDomain({ ...options, client: this.client });

  removeManagedDomain = (options: Parameters<typeof sdk.removeManagedDomain>[0]) =>
    sdk.removeManagedDomain({ ...options, client: this.client });

  verifyManagedDomain = (options: Parameters<typeof sdk.verifyManagedDomain>[0]) =>
    sdk.verifyManagedDomain({ ...options, client: this.client });

  // DNS Lookup
  lookupARecords = (options: Parameters<typeof sdk.lookupDnsARecords>[0]) =>
    sdk.lookupDnsARecords({ ...options, client: this.client });
}

class Domains {
  constructor(private client: Client) { }

  // SSL/TLS Domains
  list = (options?: Parameters<typeof sdk.listDomains>[0]) =>
    sdk.listDomains({ ...options, client: this.client });

  create = (options: Parameters<typeof sdk.createDomain>[0]) =>
    sdk.createDomain({ ...options, client: this.client });

  delete = (options: Parameters<typeof sdk.deleteDomain>[0]) =>
    sdk.deleteDomain({ ...options, client: this.client });

  getByHost = (options: Parameters<typeof sdk.getDomainByHost>[0]) =>
    sdk.getDomainByHost({ ...options, client: this.client });

  getById = (options: Parameters<typeof sdk.getDomainById>[0]) =>
    sdk.getDomainById({ ...options, client: this.client });

  checkStatus = (options: Parameters<typeof sdk.checkDomainStatus>[0]) =>
    sdk.checkDomainStatus({ ...options, client: this.client });

  provision = (options: Parameters<typeof sdk.provisionDomain>[0]) =>
    sdk.provisionDomain({ ...options, client: this.client });

  renew = (options: Parameters<typeof sdk.renewDomain>[0]) =>
    sdk.renewDomain({ ...options, client: this.client });

  // Domain Orders
  getOrder = (options: Parameters<typeof sdk.getDomainOrder>[0]) =>
    sdk.getDomainOrder({ ...options, client: this.client });

  createOrRecreateOrder = (options: Parameters<typeof sdk.createOrRecreateOrder>[0]) =>
    sdk.createOrRecreateOrder({ ...options, client: this.client });

  cancelOrder = (options: Parameters<typeof sdk.cancelDomainOrder>[0]) =>
    sdk.cancelDomainOrder({ ...options, client: this.client });

  finalizeOrder = (options: Parameters<typeof sdk.finalizeOrder>[0]) =>
    sdk.finalizeOrder({ ...options, client: this.client });

  getChallengeToken = (options: Parameters<typeof sdk.getChallengeToken>[0]) =>
    sdk.getChallengeToken({ ...options, client: this.client });

  getHttpChallengeDebug = (options: Parameters<typeof sdk.getHttpChallengeDebug>[0]) =>
    sdk.getHttpChallengeDebug({ ...options, client: this.client });

  listOrders = (options?: Parameters<typeof sdk.listOrders>[0]) =>
    sdk.listOrders({ ...options, client: this.client });
}

class Email {
  constructor(private client: Client) { }

  // Email Providers
  listProviders = (options?: Parameters<typeof sdk.listProviders2>[0]) =>
    sdk.listProviders2({ ...options, client: this.client });

  createProvider = (options: Parameters<typeof sdk.createProvider2>[0]) =>
    sdk.createProvider2({ ...options, client: this.client });

  deleteProvider = (options: Parameters<typeof sdk.deleteProvider2>[0]) =>
    sdk.deleteProvider2({ ...options, client: this.client });

  getProvider = (options: Parameters<typeof sdk.getProvider2>[0]) =>
    sdk.getProvider2({ ...options, client: this.client });

  testProvider = (options: Parameters<typeof sdk.testProvider>[0]) =>
    sdk.testProvider({ ...options, client: this.client });

  // Email Domains
  listDomains = (options?: Parameters<typeof sdk.listDomains2>[0]) =>
    sdk.listDomains2({ ...options, client: this.client });

  createDomain = (options: Parameters<typeof sdk.createDomain2>[0]) =>
    sdk.createDomain2({ ...options, client: this.client });

  deleteDomain = (options: Parameters<typeof sdk.deleteDomain2>[0]) =>
    sdk.deleteDomain2({ ...options, client: this.client });

  getDomain = (options: Parameters<typeof sdk.getDomain>[0]) =>
    sdk.getDomain({ ...options, client: this.client });

  verifyDomain = (options: Parameters<typeof sdk.verifyDomain>[0]) =>
    sdk.verifyDomain({ ...options, client: this.client });

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

  list = (options?: Parameters<typeof sdk.listServices>[0]) =>
    sdk.listServices({ ...options, client: this.client });

  create = (options: Parameters<typeof sdk.createService>[0]) =>
    sdk.createService({ ...options, client: this.client });

  delete = (options: Parameters<typeof sdk.deleteService>[0]) =>
    sdk.deleteService({ ...options, client: this.client });

  get = (options: Parameters<typeof sdk.getService>[0]) =>
    sdk.getService({ ...options, client: this.client });

  getBySlug = (options: Parameters<typeof sdk.getServiceBySlug>[0]) =>
    sdk.getServiceBySlug({ ...options, client: this.client });

  update = (options: Parameters<typeof sdk.updateService>[0]) =>
    sdk.updateService({ ...options, client: this.client });

  start = (options: Parameters<typeof sdk.startService>[0]) =>
    sdk.startService({ ...options, client: this.client });

  stop = (options: Parameters<typeof sdk.stopService>[0]) =>
    sdk.stopService({ ...options, client: this.client });

  upgrade = (options: Parameters<typeof sdk.upgradeService>[0]) =>
    sdk.upgradeService({ ...options, client: this.client });

  import = (options: Parameters<typeof sdk.importExternalService>[0]) =>
    sdk.importExternalService({ ...options, client: this.client });

  // Service Types
  getTypes = (options?: Parameters<typeof sdk.getServiceTypes>[0]) =>
    sdk.getServiceTypes({ ...options, client: this.client });

  getTypeParameters = (options: Parameters<typeof sdk.getServiceTypeParameters>[0]) =>
    sdk.getServiceTypeParameters({ ...options, client: this.client });

  // Environment Variables
  getEnvironmentVariables = (options: Parameters<typeof sdk.getServiceEnvironmentVariables>[0]) =>
    sdk.getServiceEnvironmentVariables({ ...options, client: this.client });

  getEnvironmentVariable = (options: Parameters<typeof sdk.getServiceEnvironmentVariable>[0]) =>
    sdk.getServiceEnvironmentVariable({ ...options, client: this.client });

  // Project linking
  listProjects = (options: Parameters<typeof sdk.listServiceProjects>[0]) =>
    sdk.listServiceProjects({ ...options, client: this.client });

  linkToProject = (options: Parameters<typeof sdk.linkServiceToProject>[0]) =>
    sdk.linkServiceToProject({ ...options, client: this.client });

  unlinkFromProject = (options: Parameters<typeof sdk.unlinkServiceFromProject>[0]) =>
    sdk.unlinkServiceFromProject({ ...options, client: this.client });

  listForProject = (options: Parameters<typeof sdk.listProjectServices>[0]) =>
    sdk.listProjectServices({ ...options, client: this.client });

  getProjectEnvironmentVariables = (options: Parameters<typeof sdk.getProjectServiceEnvironmentVariables>[0]) =>
    sdk.getProjectServiceEnvironmentVariables({ ...options, client: this.client });

  // Containers
  listAvailableContainers = (options?: Parameters<typeof sdk.listAvailableContainers>[0]) =>
    sdk.listAvailableContainers({ ...options, client: this.client });

  // Providers Metadata
  getProvidersMetadata = (options?: Parameters<typeof sdk.getProvidersMetadata>[0]) =>
    sdk.getProvidersMetadata({ ...options, client: this.client });

  getProviderMetadata = (options: Parameters<typeof sdk.getProviderMetadata>[0]) =>
    sdk.getProviderMetadata({ ...options, client: this.client });
}

class Files {
  constructor(private client: Client) { }

  get = (options: Parameters<typeof sdk.getFile>[0]) =>
    sdk.getFile({ ...options, client: this.client });
}

class Funnels {
  constructor(private client: Client) { }

  list = (options: Parameters<typeof sdk.listFunnels>[0]) =>
    sdk.listFunnels({ ...options, client: this.client });

  create = (options: Parameters<typeof sdk.createFunnel>[0]) =>
    sdk.createFunnel({ ...options, client: this.client });

  delete = (options: Parameters<typeof sdk.deleteFunnel>[0]) =>
    sdk.deleteFunnel({ ...options, client: this.client });

  update = (options: Parameters<typeof sdk.updateFunnel>[0]) =>
    sdk.updateFunnel({ ...options, client: this.client });

  getMetrics = (options: Parameters<typeof sdk.getFunnelMetrics>[0]) =>
    sdk.getFunnelMetrics({ ...options, client: this.client });

  previewMetrics = (options: Parameters<typeof sdk.previewFunnelMetrics>[0]) =>
    sdk.previewFunnelMetrics({ ...options, client: this.client });
}

class Git {
  constructor(private client: Client) { }

  // Git Providers
  listProviders = (options?: Parameters<typeof sdk.listGitProviders>[0]) =>
    sdk.listGitProviders({ ...options, client: this.client });

  createProvider = (options: Parameters<typeof sdk.createGitProvider>[0]) =>
    sdk.createGitProvider({ ...options, client: this.client });

  createGithubPatProvider = (options: Parameters<typeof sdk.createGithubPatProvider>[0]) =>
    sdk.createGithubPatProvider({ ...options, client: this.client });

  createGitlabOauthProvider = (options: Parameters<typeof sdk.createGitlabOauthProvider>[0]) =>
    sdk.createGitlabOauthProvider({ ...options, client: this.client });

  createGitlabPatProvider = (options: Parameters<typeof sdk.createGitlabPatProvider>[0]) =>
    sdk.createGitlabPatProvider({ ...options, client: this.client });

  deleteProvider = (options: Parameters<typeof sdk.deleteProvider3>[0]) =>
    sdk.deleteProvider3({ ...options, client: this.client });

  getProvider = (options: Parameters<typeof sdk.getGitProvider>[0]) =>
    sdk.getGitProvider({ ...options, client: this.client });

  activateProvider = (options: Parameters<typeof sdk.activateProvider>[0]) =>
    sdk.activateProvider({ ...options, client: this.client });

  deactivateProvider = (options: Parameters<typeof sdk.deactivateProvider>[0]) =>
    sdk.deactivateProvider({ ...options, client: this.client });

  getProviderConnections = (options: Parameters<typeof sdk.getProviderConnections>[0]) =>
    sdk.getProviderConnections({ ...options, client: this.client });

  checkProviderDeletionSafety = (options: Parameters<typeof sdk.checkProviderDeletionSafety>[0]) =>
    sdk.checkProviderDeletionSafety({ ...options, client: this.client });

  deleteProviderSafely = (options: Parameters<typeof sdk.deleteProviderSafely>[0]) =>
    sdk.deleteProviderSafely({ ...options, client: this.client });

  startProviderOauth = (options: Parameters<typeof sdk.startGitProviderOauth>[0]) =>
    sdk.startGitProviderOauth({ ...options, client: this.client });

  handleProviderOauthCallback = (options: Parameters<typeof sdk.handleGitProviderOauthCallback>[0]) =>
    sdk.handleGitProviderOauthCallback({ ...options, client: this.client });

  // Connections
  listConnections = (options?: Parameters<typeof sdk.listConnections>[0]) =>
    sdk.listConnections({ ...options, client: this.client });

  deleteConnection = (options: Parameters<typeof sdk.deleteConnection>[0]) =>
    sdk.deleteConnection({ ...options, client: this.client });

  activateConnection = (options: Parameters<typeof sdk.activateConnection>[0]) =>
    sdk.activateConnection({ ...options, client: this.client });

  deactivateConnection = (options: Parameters<typeof sdk.deactivateConnection>[0]) =>
    sdk.deactivateConnection({ ...options, client: this.client });

  listRepositoriesByConnection = (options: Parameters<typeof sdk.listRepositoriesByConnection>[0]) =>
    sdk.listRepositoriesByConnection({ ...options, client: this.client });

  syncRepositories = (options: Parameters<typeof sdk.syncRepositories>[0]) =>
    sdk.syncRepositories({ ...options, client: this.client });

  updateConnectionToken = (options: Parameters<typeof sdk.updateConnectionToken>[0]) =>
    sdk.updateConnectionToken({ ...options, client: this.client });

  validateConnection = (options: Parameters<typeof sdk.validateConnection>[0]) =>
    sdk.validateConnection({ ...options, client: this.client });
}

class LoadBalancer {
  constructor(private client: Client) { }

  listRoutes = (options?: Parameters<typeof sdk.listRoutes>[0]) =>
    sdk.listRoutes({ ...options, client: this.client });

  createRoute = (options: Parameters<typeof sdk.createRoute>[0]) =>
    sdk.createRoute({ ...options, client: this.client });

  deleteRoute = (options: Parameters<typeof sdk.deleteRoute>[0]) =>
    sdk.deleteRoute({ ...options, client: this.client });

  getRoute = (options: Parameters<typeof sdk.getRoute>[0]) =>
    sdk.getRoute({ ...options, client: this.client });

  updateRoute = (options: Parameters<typeof sdk.updateRoute>[0]) =>
    sdk.updateRoute({ ...options, client: this.client });
}

class Monitoring {
  constructor(private client: Client) { }

  // Monitors
  listMonitors = (options: Parameters<typeof sdk.listMonitors>[0]) =>
    sdk.listMonitors({ ...options, client: this.client });

  createMonitor = (options: Parameters<typeof sdk.createMonitor>[0]) =>
    sdk.createMonitor({ ...options, client: this.client });

  deleteMonitor = (options: Parameters<typeof sdk.deleteMonitor>[0]) =>
    sdk.deleteMonitor({ ...options, client: this.client });

  getMonitor = (options: Parameters<typeof sdk.getMonitor>[0]) =>
    sdk.getMonitor({ ...options, client: this.client });

  getBucketedStatus = (options: Parameters<typeof sdk.getBucketedStatus>[0]) =>
    sdk.getBucketedStatus({ ...options, client: this.client });

  getCurrentStatus = (options: Parameters<typeof sdk.getCurrentMonitorStatus>[0]) =>
    sdk.getCurrentMonitorStatus({ ...options, client: this.client });

  getUptimeHistory = (options: Parameters<typeof sdk.getUptimeHistory>[0]) =>
    sdk.getUptimeHistory({ ...options, client: this.client });

  // Incidents
  listIncidents = (options: Parameters<typeof sdk.listIncidents>[0]) =>
    sdk.listIncidents({ ...options, client: this.client });

  createIncident = (options: Parameters<typeof sdk.createIncident>[0]) =>
    sdk.createIncident({ ...options, client: this.client });

  getIncident = (options: Parameters<typeof sdk.getIncident>[0]) =>
    sdk.getIncident({ ...options, client: this.client });

  updateIncidentStatus = (options: Parameters<typeof sdk.updateIncidentStatus>[0]) =>
    sdk.updateIncidentStatus({ ...options, client: this.client });

  getIncidentUpdates = (options: Parameters<typeof sdk.getIncidentUpdates>[0]) =>
    sdk.getIncidentUpdates({ ...options, client: this.client });

  getBucketedIncidents = (options: Parameters<typeof sdk.getBucketedIncidents>[0]) =>
    sdk.getBucketedIncidents({ ...options, client: this.client });

  getStatusOverview = (options: Parameters<typeof sdk.getStatusOverview>[0]) =>
    sdk.getStatusOverview({ ...options, client: this.client });
}

class Notifications {
  constructor(private client: Client) { }

  // Preferences
  getPreferences = (options?: Parameters<typeof sdk.getPreferences>[0]) =>
    sdk.getPreferences({ ...options, client: this.client });

  updatePreferences = (options: Parameters<typeof sdk.updatePreferences>[0]) =>
    sdk.updatePreferences({ ...options, client: this.client });

  deletePreferences = (options?: Parameters<typeof sdk.deletePreferences>[0]) =>
    sdk.deletePreferences({ ...options, client: this.client });

  // Notification Providers
  listProviders = (options?: Parameters<typeof sdk.listNotificationProviders>[0]) =>
    sdk.listNotificationProviders({ ...options, client: this.client });

  createProvider = (options: Parameters<typeof sdk.createNotificationProvider>[0]) =>
    sdk.createNotificationProvider({ ...options, client: this.client });

  getProvider = (options: Parameters<typeof sdk.getNotificationProvider>[0]) =>
    sdk.getNotificationProvider({ ...options, client: this.client });

  deleteProvider = (options: Parameters<typeof sdk.deleteProvider4>[0]) =>
    sdk.deleteProvider4({ ...options, client: this.client });

  updateProvider = (options: Parameters<typeof sdk.updateProvider2>[0]) =>
    sdk.updateProvider2({ ...options, client: this.client });

  testProvider = (options: Parameters<typeof sdk.testProvider2>[0]) =>
    sdk.testProvider2({ ...options, client: this.client });

  // Email Provider
  createEmailProvider = (options: Parameters<typeof sdk.createEmailProvider>[0]) =>
    sdk.createEmailProvider({ ...options, client: this.client });

  updateEmailProvider = (options: Parameters<typeof sdk.updateEmailProvider>[0]) =>
    sdk.updateEmailProvider({ ...options, client: this.client });

  // Slack Provider
  createSlackProvider = (options: Parameters<typeof sdk.createSlackProvider>[0]) =>
    sdk.createSlackProvider({ ...options, client: this.client });

  updateSlackProvider = (options: Parameters<typeof sdk.updateSlackProvider>[0]) =>
    sdk.updateSlackProvider({ ...options, client: this.client });
}

class Performance {
  constructor(private client: Client) { }

  hasMetrics = (options: Parameters<typeof sdk.hasPerformanceMetrics>[0]) =>
    sdk.hasPerformanceMetrics({ ...options, client: this.client });

  getMetrics = (options: Parameters<typeof sdk.getPerformanceMetrics>[0]) =>
    sdk.getPerformanceMetrics({ ...options, client: this.client });

  getMetricsOverTime = (options: Parameters<typeof sdk.getMetricsOverTime>[0]) =>
    sdk.getMetricsOverTime({ ...options, client: this.client });

  getGroupedPageMetrics = (options: Parameters<typeof sdk.getGroupedPageMetrics>[0]) =>
    sdk.getGroupedPageMetrics({ ...options, client: this.client });

  recordSpeedMetrics = (options: Parameters<typeof sdk.recordSpeedMetrics>[0]) =>
    sdk.recordSpeedMetrics({ ...options, client: this.client });

  updateSpeedMetrics = (options: Parameters<typeof sdk.updateSpeedMetrics>[0]) =>
    sdk.updateSpeedMetrics({ ...options, client: this.client });
}

class Platform {
  constructor(private client: Client) { }

  getInfo = (options?: Parameters<typeof sdk.getPlatformInfo>[0]) =>
    sdk.getPlatformInfo({ ...options, client: this.client });

  getAccessInfo = (options?: Parameters<typeof sdk.getAccessInfo>[0]) =>
    sdk.getAccessInfo({ ...options, client: this.client });

  getPrivateIp = (options?: Parameters<typeof sdk.getPrivateIp>[0]) =>
    sdk.getPrivateIp({ ...options, client: this.client });

  getPublicIp = (options?: Parameters<typeof sdk.getPublicIp>[0]) =>
    sdk.getPublicIp({ ...options, client: this.client });

  getIpGeolocation = (options: Parameters<typeof sdk.getIpGeolocation>[0]) =>
    sdk.getIpGeolocation({ ...options, client: this.client });

  getActivityGraph = (options?: Parameters<typeof sdk.getActivityGraph>[0]) =>
    sdk.getActivityGraph({ ...options, client: this.client });

  listPresets = (options?: Parameters<typeof sdk.listPresets>[0]) =>
    sdk.listPresets({ ...options, client: this.client });
}

class Projects {
  constructor(private client: Client) { }

  list = (options?: Parameters<typeof sdk.getProjects>[0]) =>
    sdk.getProjects({ ...options, client: this.client });

  create = (options: Parameters<typeof sdk.createProject>[0]) =>
    sdk.createProject({ ...options, client: this.client });

  delete = (options: Parameters<typeof sdk.deleteProject>[0]) =>
    sdk.deleteProject({ ...options, client: this.client });

  get = (options: Parameters<typeof sdk.getProject>[0]) =>
    sdk.getProject({ ...options, client: this.client });

  getBySlug = (options: Parameters<typeof sdk.getProjectBySlug>[0]) =>
    sdk.getProjectBySlug({ ...options, client: this.client });

  update = (options: Parameters<typeof sdk.updateProject>[0]) =>
    sdk.updateProject({ ...options, client: this.client });

  getStatistics = (options: Parameters<typeof sdk.getProjectStatistics>[0]) =>
    sdk.getProjectStatistics({ ...options, client: this.client });

  updateSettings = (options: Parameters<typeof sdk.updateProjectSettings>[0]) =>
    sdk.updateProjectSettings({ ...options, client: this.client });

  // Deployments
  getDeployments = (options: Parameters<typeof sdk.getProjectDeployments>[0]) =>
    sdk.getProjectDeployments({ ...options, client: this.client });

  triggerPipeline = (options: Parameters<typeof sdk.triggerProjectPipeline>[0]) =>
    sdk.triggerProjectPipeline({ ...options, client: this.client });

  updateAutomaticDeploy = (options: Parameters<typeof sdk.updateAutomaticDeploy>[0]) =>
    sdk.updateAutomaticDeploy({ ...options, client: this.client });

  updateDeploymentConfig = (options: Parameters<typeof sdk.updateProjectDeploymentConfig>[0]) =>
    sdk.updateProjectDeploymentConfig({ ...options, client: this.client });

  // Git Settings
  updateGitSettings = (options: Parameters<typeof sdk.updateGitSettings>[0]) =>
    sdk.updateGitSettings({ ...options, client: this.client });

  // Environments
  getEnvironments = (options: Parameters<typeof sdk.getEnvironments>[0]) =>
    sdk.getEnvironments({ ...options, client: this.client });

  createEnvironment = (options: Parameters<typeof sdk.createEnvironment>[0]) =>
    sdk.createEnvironment({ ...options, client: this.client });

  deleteEnvironment = (options: Parameters<typeof sdk.deleteEnvironment>[0]) =>
    sdk.deleteEnvironment({ ...options, client: this.client });

  getEnvironment = (options: Parameters<typeof sdk.getEnvironment>[0]) =>
    sdk.getEnvironment({ ...options, client: this.client });

  updateEnvironmentSettings = (options: Parameters<typeof sdk.updateEnvironmentSettings>[0]) =>
    sdk.updateEnvironmentSettings({ ...options, client: this.client });

  teardownEnvironment = (options: Parameters<typeof sdk.teardownEnvironment>[0]) =>
    sdk.teardownEnvironment({ ...options, client: this.client });

  // Environment Domains
  getEnvironmentDomains = (options: Parameters<typeof sdk.getEnvironmentDomains>[0]) =>
    sdk.getEnvironmentDomains({ ...options, client: this.client });

  addEnvironmentDomain = (options: Parameters<typeof sdk.addEnvironmentDomain>[0]) =>
    sdk.addEnvironmentDomain({ ...options, client: this.client });

  deleteEnvironmentDomain = (options: Parameters<typeof sdk.deleteEnvironmentDomain>[0]) =>
    sdk.deleteEnvironmentDomain({ ...options, client: this.client });

  // Environment Variables
  getEnvironmentVariables = (options: Parameters<typeof sdk.getEnvironmentVariables>[0]) =>
    sdk.getEnvironmentVariables({ ...options, client: this.client });

  createEnvironmentVariable = (options: Parameters<typeof sdk.createEnvironmentVariable>[0]) =>
    sdk.createEnvironmentVariable({ ...options, client: this.client });

  deleteEnvironmentVariable = (options: Parameters<typeof sdk.deleteEnvironmentVariable>[0]) =>
    sdk.deleteEnvironmentVariable({ ...options, client: this.client });

  updateEnvironmentVariable = (options: Parameters<typeof sdk.updateEnvironmentVariable>[0]) =>
    sdk.updateEnvironmentVariable({ ...options, client: this.client });

  getEnvironmentVariableValue = (options: Parameters<typeof sdk.getEnvironmentVariableValue>[0]) =>
    sdk.getEnvironmentVariableValue({ ...options, client: this.client });

  // Custom Domains
  listCustomDomains = (options: Parameters<typeof sdk.listCustomDomainsForProject>[0]) =>
    sdk.listCustomDomainsForProject({ ...options, client: this.client });

  createCustomDomain = (options: Parameters<typeof sdk.createCustomDomain>[0]) =>
    sdk.createCustomDomain({ ...options, client: this.client });

  deleteCustomDomain = (options: Parameters<typeof sdk.deleteCustomDomain>[0]) =>
    sdk.deleteCustomDomain({ ...options, client: this.client });

  getCustomDomain = (options: Parameters<typeof sdk.getCustomDomain>[0]) =>
    sdk.getCustomDomain({ ...options, client: this.client });

  updateCustomDomain = (options: Parameters<typeof sdk.updateCustomDomain>[0]) =>
    sdk.updateCustomDomain({ ...options, client: this.client });

  linkCustomDomainToCertificate = (options: Parameters<typeof sdk.linkCustomDomainToCertificate>[0]) =>
    sdk.linkCustomDomainToCertificate({ ...options, client: this.client });

  // Containers
  listContainers = (options: Parameters<typeof sdk.listContainers>[0]) =>
    sdk.listContainers({ ...options, client: this.client });

  getContainerDetail = (options: Parameters<typeof sdk.getContainerDetail>[0]) =>
    sdk.getContainerDetail({ ...options, client: this.client });

  getContainerLogs = (options: Parameters<typeof sdk.getContainerLogs>[0]) =>
    sdk.getContainerLogs({ ...options, client: this.client });

  getContainerLogsById = (options: Parameters<typeof sdk.getContainerLogsById>[0]) =>
    sdk.getContainerLogsById({ ...options, client: this.client });

  getContainerMetrics = (options: Parameters<typeof sdk.getContainerMetrics>[0]) =>
    sdk.getContainerMetrics({ ...options, client: this.client });

  streamContainerMetrics = (options: Parameters<typeof sdk.streamContainerMetrics>[0]) =>
    sdk.streamContainerMetrics({ ...options, client: this.client });

  restartContainer = (options: Parameters<typeof sdk.restartContainer>[0]) =>
    sdk.restartContainer({ ...options, client: this.client });

  startContainer = (options: Parameters<typeof sdk.startContainer>[0]) =>
    sdk.startContainer({ ...options, client: this.client });

  stopContainer = (options: Parameters<typeof sdk.stopContainer>[0]) =>
    sdk.stopContainer({ ...options, client: this.client });

  // Analytics
  getActiveVisitors = (options: Parameters<typeof sdk.getActiveVisitors2>[0]) =>
    sdk.getActiveVisitors2({ ...options, client: this.client });

  getAggregatedBuckets = (options: Parameters<typeof sdk.getAggregatedBuckets>[0]) =>
    sdk.getAggregatedBuckets({ ...options, client: this.client });

  getHourlyVisits = (options: Parameters<typeof sdk.getHourlyVisits>[0]) =>
    sdk.getHourlyVisits({ ...options, client: this.client });

  getUniqueCounts = (options: Parameters<typeof sdk.getUniqueCounts>[0]) =>
    sdk.getUniqueCounts({ ...options, client: this.client });

  // Errors
  getErrorDashboardStats = (options: Parameters<typeof sdk.getErrorDashboardStats>[0]) =>
    sdk.getErrorDashboardStats({ ...options, client: this.client });

  listErrorGroups = (options: Parameters<typeof sdk.listErrorGroups>[0]) =>
    sdk.listErrorGroups({ ...options, client: this.client });

  getErrorGroup = (options: Parameters<typeof sdk.getErrorGroup>[0]) =>
    sdk.getErrorGroup({ ...options, client: this.client });

  updateErrorGroup = (options: Parameters<typeof sdk.updateErrorGroup>[0]) =>
    sdk.updateErrorGroup({ ...options, client: this.client });

  listErrorEvents = (options: Parameters<typeof sdk.listErrorEvents>[0]) =>
    sdk.listErrorEvents({ ...options, client: this.client });

  getErrorEvent = (options: Parameters<typeof sdk.getErrorEvent>[0]) =>
    sdk.getErrorEvent({ ...options, client: this.client });

  getErrorStats = (options: Parameters<typeof sdk.getErrorStats>[0]) =>
    sdk.getErrorStats({ ...options, client: this.client });

  getErrorTimeSeries = (options: Parameters<typeof sdk.getErrorTimeSeries>[0]) =>
    sdk.getErrorTimeSeries({ ...options, client: this.client });

  hasErrorGroups = (options: Parameters<typeof sdk.hasErrorGroups>[0]) =>
    sdk.hasErrorGroups({ ...options, client: this.client });

  hasAnalyticsEvents = (options: Parameters<typeof sdk.hasAnalyticsEvents>[0]) =>
    sdk.hasAnalyticsEvents({ ...options, client: this.client });

  // Events
  getEventsCount = (options: Parameters<typeof sdk.getEventsCount2>[0]) =>
    sdk.getEventsCount2({ ...options, client: this.client });

  getEventTypeBreakdown = (options: Parameters<typeof sdk.getEventTypeBreakdown>[0]) =>
    sdk.getEventTypeBreakdown({ ...options, client: this.client });

  getPropertyBreakdown = (options: Parameters<typeof sdk.getPropertyBreakdown>[0]) =>
    sdk.getPropertyBreakdown({ ...options, client: this.client });

  getPropertyTimeline = (options: Parameters<typeof sdk.getPropertyTimeline>[0]) =>
    sdk.getPropertyTimeline({ ...options, client: this.client });

  getEventsTimeline = (options: Parameters<typeof sdk.getEventsTimeline>[0]) =>
    sdk.getEventsTimeline({ ...options, client: this.client });

  getUniqueEvents = (options: Parameters<typeof sdk.getUniqueEvents>[0]) =>
    sdk.getUniqueEvents({ ...options, client: this.client });

  // Session Replay
  getSessionReplays = (options: Parameters<typeof sdk.getProjectSessionReplays>[0]) =>
    sdk.getProjectSessionReplays({ ...options, client: this.client });

  getSessionEvents = (options: Parameters<typeof sdk.getSessionEvents2>[0]) =>
    sdk.getSessionEvents2({ ...options, client: this.client });

  // External Images
  listExternalImages = (options: Parameters<typeof sdk.listExternalImages>[0]) =>
    sdk.listExternalImages({ ...options, client: this.client });

  pushExternalImage = (options: Parameters<typeof sdk.pushExternalImage>[0]) =>
    sdk.pushExternalImage({ ...options, client: this.client });

  getExternalImage = (options: Parameters<typeof sdk.getExternalImage>[0]) =>
    sdk.getExternalImage({ ...options, client: this.client });

  // Webhooks
  listWebhooks = (options: Parameters<typeof sdk.listWebhooks>[0]) =>
    sdk.listWebhooks({ ...options, client: this.client });

  createWebhook = (options: Parameters<typeof sdk.createWebhook>[0]) =>
    sdk.createWebhook({ ...options, client: this.client });

  deleteWebhook = (options: Parameters<typeof sdk.deleteWebhook>[0]) =>
    sdk.deleteWebhook({ ...options, client: this.client });

  getWebhook = (options: Parameters<typeof sdk.getWebhook>[0]) =>
    sdk.getWebhook({ ...options, client: this.client });

  updateWebhook = (options: Parameters<typeof sdk.updateWebhook>[0]) =>
    sdk.updateWebhook({ ...options, client: this.client });

  listDeliveries = (options: Parameters<typeof sdk.listDeliveries>[0]) =>
    sdk.listDeliveries({ ...options, client: this.client });

  getDelivery = (options: Parameters<typeof sdk.getDelivery>[0]) =>
    sdk.getDelivery({ ...options, client: this.client });

  retryDelivery = (options: Parameters<typeof sdk.retryDelivery>[0]) =>
    sdk.retryDelivery({ ...options, client: this.client });

  // DSN (Sentry-compatible)
  listDsns = (options: Parameters<typeof sdk.listDsns>[0]) =>
    sdk.listDsns({ ...options, client: this.client });

  createDsn = (options: Parameters<typeof sdk.createDsn>[0]) =>
    sdk.createDsn({ ...options, client: this.client });

  getOrCreateDsn = (options: Parameters<typeof sdk.getOrCreateDsn>[0]) =>
    sdk.getOrCreateDsn({ ...options, client: this.client });

  regenerateDsn = (options: Parameters<typeof sdk.regenerateDsn>[0]) =>
    sdk.regenerateDsn({ ...options, client: this.client });

  revokeDsn = (options: Parameters<typeof sdk.revokeDsn>[0]) =>
    sdk.revokeDsn({ ...options, client: this.client });

  // IP Access Control
  listIpAccessControl = (options: Parameters<typeof sdk.listIpAccessControl>[0]) =>
    sdk.listIpAccessControl({ ...options, client: this.client });

  createIpAccessControl = (options: Parameters<typeof sdk.createIpAccessControl>[0]) =>
    sdk.createIpAccessControl({ ...options, client: this.client });

  deleteIpAccessControl = (options: Parameters<typeof sdk.deleteIpAccessControl>[0]) =>
    sdk.deleteIpAccessControl({ ...options, client: this.client });

  getIpAccessControl = (options: Parameters<typeof sdk.getIpAccessControl>[0]) =>
    sdk.getIpAccessControl({ ...options, client: this.client });

  updateIpAccessControl = (options: Parameters<typeof sdk.updateIpAccessControl>[0]) =>
    sdk.updateIpAccessControl({ ...options, client: this.client });

  checkIpBlocked = (options: Parameters<typeof sdk.checkIpBlocked>[0]) =>
    sdk.checkIpBlocked({ ...options, client: this.client });
}

class ProxyLogs {
  constructor(private client: Client) { }

  list = (options: Parameters<typeof sdk.getProxyLogs>[0]) =>
    sdk.getProxyLogs({ ...options, client: this.client });

  getByRequestId = (options: Parameters<typeof sdk.getProxyLogByRequestId>[0]) =>
    sdk.getProxyLogByRequestId({ ...options, client: this.client });

  getById = (options: Parameters<typeof sdk.getProxyLogById>[0]) =>
    sdk.getProxyLogById({ ...options, client: this.client });

  getTimeBucketStats = (options: Parameters<typeof sdk.getTimeBucketStats>[0]) =>
    sdk.getTimeBucketStats({ ...options, client: this.client });

  getTodayStats = (options: Parameters<typeof sdk.getTodayStats>[0]) =>
    sdk.getTodayStats({ ...options, client: this.client });
}

class Repositories {
  constructor(private client: Client) { }

  listSynced = (options?: Parameters<typeof sdk.listSyncedRepositories>[0]) =>
    sdk.listSyncedRepositories({ ...options, client: this.client });

  getByName = (options: Parameters<typeof sdk.getRepositoryByName>[0]) =>
    sdk.getRepositoryByName({ ...options, client: this.client });

  getAllByName = (options: Parameters<typeof sdk.getAllRepositoriesByName>[0]) =>
    sdk.getAllRepositoriesByName({ ...options, client: this.client });

  getPresetByName = (options: Parameters<typeof sdk.getRepositoryPresetByName>[0]) =>
    sdk.getRepositoryPresetByName({ ...options, client: this.client });

  getBranches = (options: Parameters<typeof sdk.getRepositoryBranches>[0]) =>
    sdk.getRepositoryBranches({ ...options, client: this.client });

  getTags = (options: Parameters<typeof sdk.getRepositoryTags>[0]) =>
    sdk.getRepositoryTags({ ...options, client: this.client });

  getPresetLive = (options: Parameters<typeof sdk.getRepositoryPresetLive>[0]) =>
    sdk.getRepositoryPresetLive({ ...options, client: this.client });

  getBranchesById = (options: Parameters<typeof sdk.getBranchesByRepositoryId>[0]) =>
    sdk.getBranchesByRepositoryId({ ...options, client: this.client });

  checkCommitExists = (options: Parameters<typeof sdk.checkCommitExists>[0]) =>
    sdk.checkCommitExists({ ...options, client: this.client });

  getTagsById = (options: Parameters<typeof sdk.getTagsByRepositoryId>[0]) =>
    sdk.getTagsByRepositoryId({ ...options, client: this.client });
}

class SessionReplay {
  constructor(private client: Client) { }

  init = (options: Parameters<typeof sdk.initSessionReplay>[0]) =>
    sdk.initSessionReplay({ ...options, client: this.client });

  addEvents = (options: Parameters<typeof sdk.addSessionReplayEvents>[0]) =>
    sdk.addSessionReplayEvents({ ...options, client: this.client });

  addEventsLegacy = (options: Parameters<typeof sdk.addEvents>[0]) =>
    sdk.addEvents({ ...options, client: this.client });

  delete = (options: Parameters<typeof sdk.deleteSessionReplay>[0]) =>
    sdk.deleteSessionReplay({ ...options, client: this.client });

  get = (options: Parameters<typeof sdk.getSessionReplay>[0]) =>
    sdk.getSessionReplay({ ...options, client: this.client });

  getEvents = (options: Parameters<typeof sdk.getSessionReplayEvents>[0]) =>
    sdk.getSessionReplayEvents({ ...options, client: this.client });

  updateDuration = (options: Parameters<typeof sdk.updateSessionDuration>[0]) =>
    sdk.updateSessionDuration({ ...options, client: this.client });

  getVisitorSessions = (options: Parameters<typeof sdk.getVisitorSessions2>[0]) =>
    sdk.getVisitorSessions2({ ...options, client: this.client });

  listEventTypes = (options?: Parameters<typeof sdk.listEventTypes>[0]) =>
    sdk.listEventTypes({ ...options, client: this.client });
}

class Settings {
  constructor(private client: Client) { }

  get = (options?: Parameters<typeof sdk.getSettings>[0]) =>
    sdk.getSettings({ ...options, client: this.client });

  update = (options: Parameters<typeof sdk.updateSettings>[0]) =>
    sdk.updateSettings({ ...options, client: this.client });

  triggerWeeklyDigest = (options?: Parameters<typeof sdk.triggerWeeklyDigest>[0]) =>
    sdk.triggerWeeklyDigest({ ...options, client: this.client });
}

class Users {
  constructor(private client: Client) { }

  list = (options: Parameters<typeof sdk.listUsers>[0]) =>
    sdk.listUsers({ ...options, client: this.client });

  create = (options: Parameters<typeof sdk.createUser>[0]) =>
    sdk.createUser({ ...options, client: this.client });

  delete = (options: Parameters<typeof sdk.deleteUser>[0]) =>
    sdk.deleteUser({ ...options, client: this.client });

  update = (options: Parameters<typeof sdk.updateUser>[0]) =>
    sdk.updateUser({ ...options, client: this.client });

  restore = (options: Parameters<typeof sdk.restoreUser>[0]) =>
    sdk.restoreUser({ ...options, client: this.client });

  getCurrentUser = (options?: Parameters<typeof sdk.getCurrentUser>[0]) =>
    sdk.getCurrentUser({ ...options, client: this.client });

  updateSelf = (options: Parameters<typeof sdk.updateSelf>[0]) =>
    sdk.updateSelf({ ...options, client: this.client });

  // MFA
  setupMfa = (options?: Parameters<typeof sdk.setupMfa>[0]) =>
    sdk.setupMfa({ ...options, client: this.client });

  verifyAndEnableMfa = (options: Parameters<typeof sdk.verifyAndEnableMfa>[0]) =>
    sdk.verifyAndEnableMfa({ ...options, client: this.client });

  disableMfa = (options: Parameters<typeof sdk.disableMfa>[0]) =>
    sdk.disableMfa({ ...options, client: this.client });

  // Roles
  assignRole = (options: Parameters<typeof sdk.assignRole>[0]) =>
    sdk.assignRole({ ...options, client: this.client });

  removeRole = (options: Parameters<typeof sdk.removeRole>[0]) =>
    sdk.removeRole({ ...options, client: this.client });
}

export default TempsClient;
