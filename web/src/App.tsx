import { CommandPalette } from '@/components/command/CommandPalette'
import {
  CompactErrorFallback,
  ErrorBoundary,
  ErrorFallback,
} from '@/components/error'
import { ThemeProvider } from '@/components/providers/ThemeProvider'
import { ThemeWrapper } from '@/components/theme/ThemeWrapper'
import { ProjectsProvider } from '@/contexts/ProjectsContext'
import { PresetProvider } from '@/contexts/PresetContext'
import { PluginsProvider } from '@/contexts/PluginsContext'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { Loader2 } from 'lucide-react'
import { lazy, Suspense } from 'react'
import { BrowserRouter, Navigate, Route, Routes } from 'react-router-dom'
import { toast, Toaster } from 'sonner'
import { ProblemDetails } from './api/client'
import { client } from './api/client/client.gen'
import { Header } from './components/dashboard/Header'
import AppSidebar from './components/dashboard/Sidebar'
import { ProtectedLayout } from './components/layout/ProtectedLayout'
import { SettingsLayout } from './components/settings/SettingsLayout'
import { SidebarInset, SidebarProvider } from './components/ui/sidebar'
import { AuthProvider } from './contexts/AuthContext'
import { BreadcrumbProvider } from './contexts/BreadcrumbContext'
import { PlatformAccessProvider } from './contexts/PlatformAccessContext'
import './globals.css'
import { MonitoringSettings } from './components/monitoring/MonitoringSettings'
import { AddNotificationProvider } from './pages/AddNotificationProvider'
import { EditNotificationProvider } from './pages/EditNotificationProvider'
import { Monitoring } from './pages/Monitoring'
import { PluginPage } from './pages/plugins/PluginPage'
// Lazy load all pages
const Account = lazy(() =>
  import('./pages/Account').then((m) => ({ default: m.Account }))
)
const Projects = lazy(() =>
  import('./pages/Projects').then((m) => ({ default: m.Projects }))
)
const Revenue = lazy(() =>
  import('./pages/Revenue').then((m) => ({ default: m.Revenue }))
)
const Sandboxes = lazy(() => import('./pages/Sandboxes'))
const SandboxDetail = lazy(() => import('./pages/SandboxDetail'))
const Storage = lazy(() =>
  import('./pages/Storage').then((m) => ({ default: m.Storage }))
)
const CreateService = lazy(() =>
  import('./pages/CreateServiceNew').then((m) => ({ default: m.CreateService }))
)
const ImportService = lazy(() =>
  import('./pages/ImportService').then((m) => ({ default: m.ImportService }))
)
const ServiceDetail = lazy(() =>
  import('./pages/ServiceDetail').then((m) => ({ default: m.ServiceDetail }))
)
const ServiceDataBrowser = lazy(() =>
  import('./pages/ServiceDataBrowser').then((m) => ({
    default: m.ServiceDataBrowser,
  }))
)
const ServiceRestore = lazy(() =>
  import('./pages/ServiceRestore').then((m) => ({
    default: m.ServiceRestore,
  }))
)
const MajorUpgradeDetail = lazy(() =>
  import('./pages/MajorUpgradeDetail').then((m) => ({
    default: m.MajorUpgradeDetail,
  }))
)
const AddClusterMember = lazy(() =>
  import('./pages/AddClusterMember').then((m) => ({
    default: m.AddClusterMember,
  }))
)
const Users = lazy(() =>
  import('./pages/Users').then((m) => ({ default: m.Users }))
)
const UserDetail = lazy(() =>
  import('./pages/UserDetail').then((m) => ({ default: m.UserDetail }))
)
const CustomRoutes = lazy(() =>
  import('./pages/Routes').then((m) => ({ default: m.Routes }))
)
const AddRoute = lazy(() =>
  import('./pages/AddRoute').then((m) => ({ default: m.AddRoute }))
)
const GitSources = lazy(() =>
  import('./pages/GitSources').then((m) => ({ default: m.GitSources }))
)
const AddGitProvider = lazy(() =>
  import('./pages/AddGitProvider').then((m) => ({ default: m.AddGitProvider }))
)
const GitProviderDetail = lazy(() => import('./pages/GitProviderDetail'))
const DnsProviders = lazy(() =>
  import('./pages/DnsProviders').then((m) => ({ default: m.DnsProviders }))
)
const AddDnsProvider = lazy(() =>
  import('./pages/AddDnsProvider').then((m) => ({ default: m.AddDnsProvider }))
)
const DnsProviderDetail = lazy(() => import('./pages/DnsProviderDetail'))
const Domains = lazy(() =>
  import('./pages/Domains').then((m) => ({ default: m.Domains }))
)
const AddDomain = lazy(() =>
  import('./pages/AddDomain').then((m) => ({ default: m.AddDomain }))
)
const DomainDetail = lazy(() =>
  import('./pages/DomainDetail').then((m) => ({ default: m.DomainDetail }))
)
const Backups = lazy(() =>
  import('./pages/Backups').then((m) => ({ default: m.Backups }))
)
const S3SourceDetail = lazy(() =>
  import('./pages/S3SourceDetail').then((m) => ({ default: m.S3SourceDetail }))
)
const BackupDetail = lazy(() =>
  import('./pages/BackupDetail').then((m) => ({ default: m.BackupDetail }))
)
const CreateS3Source = lazy(() =>
  import('./pages/CreateS3Source').then((m) => ({ default: m.CreateS3Source }))
)
const NewProject = lazy(() =>
  import('./pages/NewProject').then((m) => ({ default: m.NewProject }))
)
const ImportProject = lazy(() =>
  import('./pages/ImportProject').then((m) => ({ default: m.ImportProject }))
)
const Import = lazy(() => import('./pages/Import'))
const ProjectDetail = lazy(() =>
  import('./pages/ProjectDetail').then((m) => ({ default: m.ProjectDetail }))
)
const Settings = lazy(() =>
  import('./pages/Settings').then((m) => ({ default: m.Settings }))
)
const Notifications = lazy(() =>
  import('./pages/Notifications').then((m) => ({ default: m.Notifications }))
)
const Email = lazy(() =>
  import('./pages/Email').then((m) => ({ default: m.Email }))
)
const EmailDetail = lazy(() =>
  import('./pages/EmailDetail').then((m) => ({ default: m.EmailDetail }))
)
const EmailDomainDetail = lazy(() =>
  import('./pages/EmailDomainDetail').then((m) => ({
    default: m.EmailDomainDetail,
  }))
)
const ExternalConnectivitySetup = lazy(() =>
  import('./pages/ExternalConnectivitySetup').then((m) => ({
    default: m.ExternalConnectivitySetup,
  }))
)
const AuditLogs = lazy(() =>
  import('./pages/AuditLogs').then((m) => ({ default: m.AuditLogs }))
)
const CliLogin = lazy(() =>
  import('./pages/CliLogin').then((m) => ({ default: m.CliLogin }))
)
const ProxyLogs = lazy(() => import('./pages/ProxyLogs'))
const ProxyLogDetail = lazy(() => import('./pages/ProxyLogDetail'))
const IpGeolocationDetail = lazy(() => import('./pages/IpGeolocationDetail'))
const ApiKeys = lazy(() => import('./pages/ApiKeys'))
const ApiKeyCreate = lazy(() => import('./pages/ApiKeyCreate'))
const ApiKeyEdit = lazy(() => import('./pages/ApiKeyEdit'))
const ApiKeyDetail = lazy(() => import('./pages/ApiKeyDetail'))
const MfaVerify = lazy(() =>
  import('./pages/MfaVerify').then((m) => ({ default: m.MfaVerify }))
)
const NotFound = lazy(() => import('./components/global/NotFound'))

// Settings sub-pages
const DockerRegistryPage = lazy(() =>
  import('./pages/settings/DockerRegistryPage').then((m) => ({
    default: m.DockerRegistryPage,
  }))
)
const SecurityPage = lazy(() =>
  import('./pages/settings/SecurityPage').then((m) => ({
    default: m.SecurityPage,
  }))
)
const RateLimitingPage = lazy(() =>
  import('./pages/settings/RateLimitingPage').then((m) => ({
    default: m.RateLimitingPage,
  }))
)
const DiskMonitoringPage = lazy(() =>
  import('./pages/settings/DiskMonitoringPage').then((m) => ({
    default: m.DiskMonitoringPage,
  }))
)
const PluginsPage = lazy(() =>
  import('./pages/settings/PluginsPage').then((m) => ({
    default: m.PluginsPage,
  }))
)
const NodesPage = lazy(() =>
  import('./pages/settings/NodesPage').then((m) => ({
    default: m.NodesPage,
  }))
)
const NodeDetailPage = lazy(() =>
  import('./pages/settings/NodesPage').then((m) => ({
    default: m.NodeDetailPage,
  }))
)
const AiGateway = lazy(() =>
  import('./pages/AiGateway').then((m) => ({
    default: m.AiGatewayPage,
  }))
)
const AgentSandboxLayout = lazy(() =>
  import('./pages/agent-sandbox/AgentSandboxLayout').then((m) => ({
    default: m.AgentSandboxLayout,
  }))
)
const AgentSandboxDashboard = lazy(() =>
  import('./pages/agent-sandbox/AgentSandboxDashboard').then((m) => ({
    default: m.AgentSandboxDashboard,
  }))
)
const AgentSandboxProvidersList = lazy(() =>
  import('./pages/agent-sandbox/AgentSandboxProvidersList').then((m) => ({
    default: m.AgentSandboxProvidersList,
  }))
)
const AgentSandboxProviderDetail = lazy(() =>
  import('./pages/agent-sandbox/AgentSandboxProviderDetail').then((m) => ({
    default: m.AgentSandboxProviderDetail,
  }))
)
const AgentSandboxSandboxPage = lazy(() =>
  import('./pages/agent-sandbox/AgentSandboxSandboxPage').then((m) => ({
    default: m.AgentSandboxSandboxPage,
  }))
)
const AgentSandboxPreviewPage = lazy(() =>
  import('./pages/agent-sandbox/AgentSandboxPreviewPage').then((m) => ({
    default: m.AgentSandboxPreviewPage,
  }))
)
const AgentSandboxSecretsPage = lazy(() =>
  import('./pages/agent-sandbox/AgentSandboxSecretsPage').then((m) => ({
    default: m.AgentSandboxSecretsPage,
  }))
)
const GlobalSkillsSettingsPage = lazy(() =>
  import('./components/settings/GlobalSkillsSettings').then((m) => ({
    default: m.GlobalSkillsSettings,
  }))
)
const GlobalMcpServersSettingsPage = lazy(() =>
  import('./components/settings/GlobalMcpServersSettings').then((m) => ({
    default: m.GlobalMcpServersSettings,
  }))
)
const GlobalSkillDetailPage = lazy(() =>
  import('./pages/settings/GlobalSkillDetail').then((m) => ({
    default: m.GlobalSkillDetail,
  }))
)
const GlobalMcpServerDetailPage = lazy(() =>
  import('./pages/settings/GlobalMcpServerDetail').then((m) => ({
    default: m.GlobalMcpServerDetail,
  }))
)

// Loading component
const PageLoader = () => (
  <div className="flex items-center justify-center min-h-[400px]">
    <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
  </div>
)

// Full app routes with sidebar
const FullAppRoutes = () => {
  return (
    <BreadcrumbProvider>
      <SidebarProvider>
        {/* Wrap sidebar with independent error boundary */}
        <ErrorBoundary
          fallback={(error, _errorInfo, resetError) => (
            <CompactErrorFallback
              error={error}
              resetError={resetError}
              componentName="Sidebar"
            />
          )}
          onError={(error, errorInfo) => {
            console.error('[App] Sidebar error caught by boundary:', error)
            console.error('[App] Component stack:', errorInfo.componentStack)
          }}
        >
          <AppSidebar />
        </ErrorBoundary>
        <SidebarInset>
          {/* Wrap header with independent error boundary */}
          <ErrorBoundary
            fallback={(error, _errorInfo, resetError) => (
              <CompactErrorFallback
                error={error}
                resetError={resetError}
                componentName="Header"
                minimal
              />
            )}
            onError={(error, errorInfo) => {
              console.error('[App] Header error caught by boundary:', error)
              console.error('[App] Component stack:', errorInfo.componentStack)
            }}
          >
            <Header />
          </ErrorBoundary>
          {/* Wrap page content with error boundary */}
          <ErrorBoundary
            fallback={(error, errorInfo, resetError) => (
              <ErrorFallback
                error={error}
                errorInfo={errorInfo}
                resetError={resetError}
              />
            )}
            onError={(error, errorInfo) => {
              console.error('[App] Page error caught by boundary:', error)
              console.error('[App] Component stack:', errorInfo.componentStack)
            }}
          >
            <div className="h-full overflow-y-auto py-2 px-0 sm:p-4">
              <Routes>
                <Route path="/" element={<Navigate to="/projects" replace />} />
                <Route path="/dashboard" element={<Navigate to="/projects" replace />} />
                <Route path="/account" element={<Account />} />
                <Route path="/projects" element={<Projects />} />
                <Route path="/revenue" element={<Revenue />} />
                <Route path="/sandboxes" element={<Sandboxes />} />
                <Route path="/sandboxes/:sandboxId" element={<SandboxDetail />} />
                <Route path="/monitoring" element={<Monitoring />}>
                  <Route index element={<Navigate to="resources" replace />} />
                  <Route path="providers/add" element={<AddNotificationProvider />} />
                  <Route path="providers/edit/:id" element={<EditNotificationProvider />} />
                  <Route path=":section" element={<MonitoringSettings />} />
                </Route>
                {/* Observe section */}
                <Route path="/proxy-logs" element={<ProxyLogs />} />
                <Route path="/proxy-logs/:id" element={<ProxyLogDetail />} />
                <Route path="/audit-logs" element={<AuditLogs />} />
                {/* CLI device-authorization approval surface. The route
                    sits inside the protected layout so unauthenticated
                    visitors get bounced through /login and the
                    captureReturnTo() infrastructure brings them back. */}
                <Route path="/cli-login" element={<CliLogin />} />
                <Route path="/cli-login/:userCode" element={<CliLogin />} />
                {/* Settings drill-down: only items NOT surfaced at the
                    main sidebar root live here. Top-level resources
                    (domains, storage, email, AI, source providers,
                    backups) moved out so they don't trigger the
                    settings sidebar swap. */}
                <Route path="/settings" element={<SettingsLayout />}>
                  <Route index element={<Settings />} />
                  <Route path="notifications" element={<Notifications />} />
                  <Route path="users" element={<Users />} />
                  <Route path="users/:userId" element={<UserDetail />} />
                  <Route path="keys" element={<ApiKeys />} />
                  <Route path="keys/new" element={<ApiKeyCreate />} />
                  <Route path="keys/:id" element={<ApiKeyDetail />} />
                  <Route path="keys/:id/edit" element={<ApiKeyEdit />} />
                  <Route path="load-balancer" element={<CustomRoutes />} />
                  <Route path="load-balancer/add" element={<AddRoute />} />
                  <Route path="docker-registry" element={<DockerRegistryPage />} />
                  {/* Security */}
                  <Route path="security" element={<SecurityPage />} />
                  <Route path="rate-limiting" element={<RateLimitingPage />} />
                  <Route path="disk-monitoring" element={<DiskMonitoringPage />} />
                  <Route path="nodes" element={<NodesPage />} />
                  <Route path="nodes/:nodeId" element={<NodeDetailPage />} />
                  <Route path="plugins" element={<PluginsPage />} />
                </Route>
                {/* Top-level resources surfaced in the main sidebar */}
                <Route path="/domains" element={<Domains />} />
                <Route path="/domains/add" element={<AddDomain />} />
                <Route path="/domains/:id" element={<DomainDetail />} />
                <Route path="/storage" element={<Storage />} />
                <Route path="/storage/create" element={<CreateService />} />
                <Route path="/storage/import" element={<ImportService />} />
                <Route path="/storage/:id" element={<ServiceDetail />} />
                <Route path="/storage/:id/browse" element={<ServiceDataBrowser />} />
                <Route path="/storage/:id/restore" element={<ServiceRestore />} />
                <Route path="/storage/:id/upgrades/:upgradeId" element={<MajorUpgradeDetail />} />
                <Route path="/storage/:id/members/add" element={<AddClusterMember />} />
                <Route path="/email" element={<Email />} />
                <Route path="/email/domains/:id" element={<EmailDomainDetail />} />
                <Route path="/email/:id" element={<EmailDetail />} />
                <Route path="/ai-gateway" element={<AiGateway />} />
                <Route path="/agent-sandbox" element={<AgentSandboxLayout />}>
                  <Route index element={<AgentSandboxDashboard />} />
                  <Route path="providers" element={<AgentSandboxProvidersList />} />
                  <Route path="providers/:id" element={<AgentSandboxProviderDetail />} />
                  <Route path="sandbox" element={<AgentSandboxSandboxPage />} />
                  <Route path="preview" element={<AgentSandboxPreviewPage />} />
                  <Route path="secrets" element={<AgentSandboxSecretsPage />} />
                </Route>
                <Route path="/skills" element={<GlobalSkillsSettingsPage />} />
                <Route path="/skills/:slug" element={<GlobalSkillDetailPage />} />
                <Route path="/mcp-servers" element={<GlobalMcpServersSettingsPage />} />
                <Route path="/mcp-servers/:slug" element={<GlobalMcpServerDetailPage />} />
                <Route path="/git-providers" element={<GitSources />} />
                <Route path="/git-providers/add" element={<AddGitProvider />} />
                <Route path="/git-providers/:id" element={<GitProviderDetail />} />
                <Route path="/dns-providers" element={<DnsProviders />} />
                <Route path="/dns-providers/add" element={<AddDnsProvider />} />
                <Route path="/dns-providers/:id" element={<DnsProviderDetail />} />
                <Route path="/backups" element={<Backups />} />
                <Route path="/backups/s3-sources/new" element={<CreateS3Source />} />
                <Route path="/backups/s3-sources/:id" element={<S3SourceDetail />} />
                <Route path="/backups/s3-sources/:id/backups/:backupId" element={<BackupDetail />} />
                {/* Backward-compat: old /settings/<resource> links → new top-level */}
                <Route path="/settings/domains/*" element={<Navigate to="/domains" replace />} />
                <Route path="/settings/email/*" element={<Navigate to="/email" replace />} />
                <Route path="/settings/ai-gateway/*" element={<Navigate to="/ai-gateway" replace />} />
                <Route path="/settings/agent-sandbox/*" element={<Navigate to="/agent-sandbox" replace />} />
                <Route path="/settings/skills/*" element={<Navigate to="/skills" replace />} />
                <Route path="/settings/mcp-servers/*" element={<Navigate to="/mcp-servers" replace />} />
                <Route path="/settings/git-providers/*" element={<Navigate to="/git-providers" replace />} />
                <Route path="/settings/dns-providers/*" element={<Navigate to="/dns-providers" replace />} />
                <Route path="/settings/backups/*" element={<Navigate to="/backups" replace />} />
                {/* Projects */}
                <Route path="/projects/new" element={<NewProject />} />
                <Route path="/projects/import-wizard" element={<Import />} />
                <Route
                  path="/projects/import/:repositoryId"
                  element={<ImportProject />}
                />
                <Route path="/projects/:slug/*" element={<ProjectDetail />} />
                {/* Utility */}
                <Route path="/ip/:ip" element={<IpGeolocationDetail />} />
                <Route path="/setup/connectivity" element={<ExternalConnectivitySetup />} />
                {/* External plugin routes */}
                <Route path="/plugins/:pluginName/*" element={<PluginPage />} />
                <Route path="*" element={<NotFound />} />
              </Routes>
            </div>
          </ErrorBoundary>
        </SidebarInset>
        <CommandPalette />
      </SidebarProvider>
    </BreadcrumbProvider>
  )
}

const AuthenticatedRoutes = () => {
  return (
    <PlatformAccessProvider>
      <PluginsProvider>
        <FullAppRoutes />
      </PluginsProvider>
    </PlatformAccessProvider>
  )
}

const AppContent = () => {
  return (
    <BrowserRouter>
      <AuthProvider>
        <ProjectsProvider>
          <PresetProvider>
            <Suspense fallback={<PageLoader />}>
              <Routes>
                {/* Public routes that don't require authentication */}
                <Route path="/mfa-verify" element={<MfaVerify />} />

                {/* Protected routes - layout determined by demo mode */}
                <Route
                  path="/*"
                  element={
                    <ProtectedLayout>
                      <AuthenticatedRoutes />
                    </ProtectedLayout>
                  }
                />
              </Routes>
            </Suspense>
          </PresetProvider>
        </ProjectsProvider>
      </AuthProvider>
    </BrowserRouter>
  )
}

// Helper to generate friendly error titles from mutation operations
const getErrorTitle = (
  context: any,
  defaultTitle?: string
): string | undefined => {
  // Check for custom error title in mutation meta
  if (context?.meta?.errorTitle) {
    return context.meta.errorTitle
  }
  const mutationKey = context?.mutationKey?.[0]
  if (mutationKey) {
    // e.g., "createProject" -> "Failed to create project"
    return `Failed to ${mutationKey.replace(/([A-Z])/g, ' $1').toLowerCase()}`
  }

  return defaultTitle
}

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      refetchOnWindowFocus: false,
    },
    mutations: {
      onError: (error: unknown, _variables, context) => {
        const problemDetails = error as ProblemDetails

        // Get custom error title
        const customTitle = getErrorTitle(context, problemDetails.title)

        if (problemDetails.title) {
          toast.error(customTitle || problemDetails.title, {
            description: problemDetails.detail,
          })
        } else {
          toast.error(customTitle || 'An error occurred')
        }
      },
    },
  },
})
client.setConfig({ baseUrl: '/api' })

const App = () => {
  return (
    <ThemeProvider defaultTheme="system" enableSystem attribute="class">
      <ThemeWrapper>
        <QueryClientProvider client={queryClient}>
          <AppContent />
        </QueryClientProvider>
        <Toaster position="top-center" />
      </ThemeWrapper>
    </ThemeProvider>
  )
}

export default App
