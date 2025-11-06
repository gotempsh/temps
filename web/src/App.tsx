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
import { SidebarInset, SidebarProvider } from './components/ui/sidebar'
import { AuthProvider } from './contexts/AuthContext'
import { BreadcrumbProvider } from './contexts/BreadcrumbContext'
import {
  PlatformAccessProvider,
  usePlatformAccess,
} from './contexts/PlatformAccessContext'
import './globals.css'
import { MonitoringSettings } from './components/monitoring/MonitoringSettings'
import { AddNotificationProvider } from './pages/AddNotificationProvider'
import { EditNotificationProvider } from './pages/EditNotificationProvider'
import { Monitoring } from './pages/Monitoring'
// Lazy load all pages
const Dashboard = lazy(() =>
  import('./pages/Dashboard').then((m) => ({ default: m.Dashboard }))
)
const Account = lazy(() =>
  import('./pages/Account').then((m) => ({ default: m.Account }))
)
const Projects = lazy(() =>
  import('./pages/Projects').then((m) => ({ default: m.Projects }))
)
const Storage = lazy(() =>
  import('./pages/Storage').then((m) => ({ default: m.Storage }))
)
const CreateService = lazy(() =>
  import('./pages/CreateServiceNew').then((m) => ({ default: m.CreateService }))
)
const ServiceDetail = lazy(() =>
  import('./pages/ServiceDetail').then((m) => ({ default: m.ServiceDetail }))
)
const Users = lazy(() =>
  import('./pages/Users').then((m) => ({ default: m.Users }))
)
const CustomRoutes = lazy(() =>
  import('./pages/Routes').then((m) => ({ default: m.Routes }))
)
const GitSources = lazy(() =>
  import('./pages/GitSources').then((m) => ({ default: m.GitSources }))
)
const AddGitProvider = lazy(() =>
  import('./pages/AddGitProvider').then((m) => ({ default: m.AddGitProvider }))
)
const GitProviderDetail = lazy(() => import('./pages/GitProviderDetail'))
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
// const ImportTemplate = lazy(() => import('./pages/ImportTemplate').then(m => ({ default: m.ImportTemplate })))
const ProjectDetail = lazy(() =>
  import('./pages/ProjectDetail').then((m) => ({ default: m.ProjectDetail }))
)
const Settings = lazy(() =>
  import('./pages/Settings').then((m) => ({ default: m.Settings }))
)
const ExternalConnectivitySetup = lazy(() =>
  import('./pages/ExternalConnectivitySetup').then((m) => ({
    default: m.ExternalConnectivitySetup,
  }))
)
const AuditLogs = lazy(() =>
  import('./pages/AuditLogs').then((m) => ({ default: m.AuditLogs }))
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

// Loading component
const PageLoader = () => (
  <div className="flex items-center justify-center min-h-[400px]">
    <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
  </div>
)

// Component that uses the PlatformAccess context
const AppContent = () => {
  const { accessInfo, isLoading, error } = usePlatformAccess()

  // Show loading state while fetching platform info (optional - can be removed for smoother UX)
  if (isLoading && !accessInfo) {
    return (
      <div className="flex items-center justify-center min-h-screen">
        <div className="text-center space-y-2">
          <Loader2 className="h-8 w-8 animate-spin mx-auto text-muted-foreground" />
          <p className="text-sm text-muted-foreground">
            Initializing platform...
          </p>
        </div>
      </div>
    )
  }

  // Show error state if platform info fails to load (optional)
  if (error && !accessInfo) {
    console.error('[PlatformAccess] Error loading platform info:', error)
    // Continue rendering the app even if platform info fails
  }

  return (
    <BrowserRouter>
      <AuthProvider>
        <ProjectsProvider>
          <PresetProvider>
            <Suspense fallback={<PageLoader />}>
              <Routes>
                {/* Public routes that don't require authentication */}
                <Route path="/mfa-verify" element={<MfaVerify />} />

                {/* Protected routes */}
                <Route
                  path="/*"
                  element={
                    <ProtectedLayout>
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
                              console.error(
                                '[App] Sidebar error caught by boundary:',
                                error
                              )
                              console.error(
                                '[App] Component stack:',
                                errorInfo.componentStack
                              )
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
                                console.error(
                                  '[App] Header error caught by boundary:',
                                  error
                                )
                                console.error(
                                  '[App] Component stack:',
                                  errorInfo.componentStack
                                )
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
                                // Log errors to console in development
                                console.error(
                                  '[App] Page error caught by boundary:',
                                  error
                                )
                                console.error(
                                  '[App] Component stack:',
                                  errorInfo.componentStack
                                )
                                // In production, you could send to error tracking service
                                // Example: Sentry.captureException(error, { contexts: { react: { componentStack: errorInfo.componentStack } } })
                              }}
                            >
                              <div className="h-full overflow-y-auto py-2 px-0 sm:p-4">
                                <Routes>
                                  <Route
                                    path="/"
                                    element={
                                      <Navigate to="/dashboard" replace />
                                    }
                                  />
                                  <Route
                                    path="/dashboard"
                                    element={<Dashboard />}
                                  />
                                  <Route
                                    path="/account"
                                    element={<Account />}
                                  />
                                  <Route
                                    path="/projects"
                                    element={<Projects />}
                                  />
                                  <Route
                                    path="/storage"
                                    element={<Storage />}
                                  />
                                  <Route
                                    path="/storage/create"
                                    element={<CreateService />}
                                  />
                                  <Route
                                    path="/storage/:id"
                                    element={<ServiceDetail />}
                                  />
                                  <Route path="/users" element={<Users />} />
                                  <Route
                                    path="/load-balancer"
                                    element={<CustomRoutes />}
                                  />
                                  <Route
                                    path="/git-sources"
                                    element={<GitSources />}
                                  />
                                  <Route
                                    path="/git-sources/add"
                                    element={<AddGitProvider />}
                                  />
                                  <Route
                                    path="/git-providers/:id"
                                    element={<GitProviderDetail />}
                                  />
                                  <Route
                                    path="/domains"
                                    element={<Domains />}
                                  />
                                  <Route
                                    path="/domains/add"
                                    element={<AddDomain />}
                                  />
                                  <Route
                                    path="/domains/:id"
                                    element={<DomainDetail />}
                                  />
                                  <Route
                                    path="/backups"
                                    element={<Backups />}
                                  />
                                  <Route
                                    path="/backups/s3-sources/new"
                                    element={<CreateS3Source />}
                                  />
                                  <Route
                                    path="/monitoring"
                                    element={<Monitoring />}
                                  >
                                    <Route
                                      index
                                      element={
                                        <Navigate to="project" replace />
                                      }
                                    />
                                    <Route
                                      path="providers/add"
                                      element={<AddNotificationProvider />}
                                    />
                                    <Route
                                      path="providers/edit/:id"
                                      element={<EditNotificationProvider />}
                                    />
                                    <Route
                                      path=":section"
                                      element={<MonitoringSettings />}
                                    />
                                  </Route>
                                  <Route
                                    path="/backups/s3-sources/:id"
                                    element={<S3SourceDetail />}
                                  />
                                  <Route
                                    path="/backups/s3-sources/:id/backups/:backupId"
                                    element={<BackupDetail />}
                                  />
                                  <Route
                                    path="/projects/new"
                                    element={<NewProject />}
                                  />
                                  <Route
                                    path="/projects/import-wizard"
                                    element={<Import />}
                                  />
                                  <Route
                                    path="/projects/import/*"
                                    element={<ImportProject />}
                                  />
                                  {/* <Route path="/projects/template/:name/import" element={<ImportTemplate />} /> */}
                                  <Route
                                    path="/projects/:slug/*"
                                    element={<ProjectDetail />}
                                  />
                                  <Route
                                    path="/settings"
                                    element={<Settings />}
                                  />
                                  <Route
                                    path="/settings/audit-logs"
                                    element={<AuditLogs />}
                                  />
                                  <Route
                                    path="/proxy-logs"
                                    element={<ProxyLogs />}
                                  />
                                  <Route
                                    path="/proxy-logs/:id"
                                    element={<ProxyLogDetail />}
                                  />
                                  <Route
                                    path="/ip/:ip"
                                    element={<IpGeolocationDetail />}
                                  />
                                  <Route
                                    path="/setup/connectivity"
                                    element={<ExternalConnectivitySetup />}
                                  />
                                  <Route path="/keys" element={<ApiKeys />} />
                                  <Route
                                    path="/keys/new"
                                    element={<ApiKeyCreate />}
                                  />
                                  <Route
                                    path="/keys/:id"
                                    element={<ApiKeyDetail />}
                                  />
                                  <Route
                                    path="/keys/:id/edit"
                                    element={<ApiKeyEdit />}
                                  />
                                  <Route path="*" element={<NotFound />} />
                                </Routes>
                              </div>
                            </ErrorBoundary>
                          </SidebarInset>
                          <CommandPalette />
                        </SidebarProvider>
                      </BreadcrumbProvider>
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
          <PlatformAccessProvider>
            <AppContent />
          </PlatformAccessProvider>
        </QueryClientProvider>
        <Toaster position="top-center" />
      </ThemeWrapper>
    </ThemeProvider>
  )
}

export default App
