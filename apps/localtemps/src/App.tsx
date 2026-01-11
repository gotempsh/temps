import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Loader2, Play, Square, ChevronDown, ChevronUp, Trash2, Database, HardDrive, CheckCircle2, XCircle, AlertCircle, Info } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import { CopyButton } from "@/components/ui/copy-button";
import { SidebarProvider, SidebarInset, SidebarTrigger } from "@/components/ui/sidebar";
import { AppSidebar, type NavPage } from "@/components/app-sidebar";
import { AnalyticsInspector } from "@/components/analytics/AnalyticsInspector";
import { cn } from "@/lib/utils";

interface ServiceStatus {
  name: string;
  service_type: string;
  running: boolean;
  port: number | null;
  connection_info: string | null;
}

interface EnvConfig {
  api_url: string;
  token: string;
  project_id: number;
  env_vars: string;
}

interface CommandResult<T> {
  success: boolean;
  data: T | null;
  error: string | null;
}

interface ActivityLogEntry {
  timestamp: string;
  level: string;
  message: string;
  service: string | null;
}

function ServiceCard({ service }: { service: ServiceStatus }) {
  const Icon = service.service_type === "kv" ? Database : HardDrive;

  return (
    <Card className={cn(
      "transition-all duration-200",
      service.running ? "border-success/50 bg-success/5" : "border-muted"
    )}>
      <CardHeader className="pb-3">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            <div className={cn(
              "p-2 rounded-lg",
              service.running ? "bg-success/10 text-success" : "bg-muted text-muted-foreground"
            )}>
              <Icon className="h-5 w-5" />
            </div>
            <div>
              <CardTitle className="text-base">{service.name}</CardTitle>
              <CardDescription className="text-xs uppercase tracking-wide">
                {service.service_type}
              </CardDescription>
            </div>
          </div>
          <Badge variant={service.running ? "success" : "secondary"}>
            {service.running ? "Running" : "Stopped"}
          </Badge>
        </div>
      </CardHeader>
      {service.running && service.connection_info && (
        <CardContent className="pt-0">
          <div className="flex items-center gap-2 p-2 rounded-md bg-muted/50 font-mono text-xs">
            <code className="flex-1 truncate">{service.connection_info}</code>
            <CopyButton value={service.connection_info} size="sm" variant="ghost" className="h-7 w-7 p-0" />
          </div>
          {service.port && (
            <p className="text-xs text-muted-foreground mt-2">Port: {service.port}</p>
          )}
        </CardContent>
      )}
    </Card>
  );
}

function EnvVarsSection({ config }: { config: EnvConfig }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-lg">Environment Variables</CardTitle>
        <CardDescription>
          Add these to your <code className="text-xs bg-muted px-1 py-0.5 rounded">.env</code> file to use the Temps SDK with LocalTemps
        </CardDescription>
      </CardHeader>
      <CardContent>
        <div className="relative">
          <pre className="p-4 rounded-lg bg-muted font-mono text-sm overflow-x-auto">
            {config.env_vars}
          </pre>
          <CopyButton
            value={config.env_vars}
            className="absolute top-2 right-2"
          >
            Copy
          </CopyButton>
        </div>
      </CardContent>
    </Card>
  );
}

function ActivityLog({ logs, onClear }: { logs: ActivityLogEntry[]; onClear: () => void }) {
  const [isExpanded, setIsExpanded] = useState(false);

  const formatTime = (timestamp: string) => {
    const date = new Date(timestamp);
    return date.toLocaleTimeString();
  };

  const getLevelIcon = (level: string) => {
    switch (level) {
      case 'error': return <XCircle className="h-4 w-4 text-destructive" />;
      case 'success': return <CheckCircle2 className="h-4 w-4 text-success" />;
      case 'warn': return <AlertCircle className="h-4 w-4 text-warning" />;
      default: return <Info className="h-4 w-4 text-muted-foreground" />;
    }
  };

  const getLevelClass = (level: string) => {
    switch (level) {
      case 'error': return 'text-destructive';
      case 'success': return 'text-success';
      case 'warn': return 'text-warning';
      default: return 'text-foreground';
    }
  };

  return (
    <Card>
      <Collapsible open={isExpanded} onOpenChange={setIsExpanded}>
        <CardHeader className="pb-3">
          <div className="flex items-center justify-between">
            <CardTitle className="text-lg">Activity Log</CardTitle>
            <div className="flex items-center gap-2">
              <Button variant="ghost" size="sm" onClick={onClear}>
                <Trash2 className="h-4 w-4" />
              </Button>
              <CollapsibleTrigger asChild>
                <Button variant="ghost" size="sm">
                  {isExpanded ? <ChevronUp className="h-4 w-4" /> : <ChevronDown className="h-4 w-4" />}
                </Button>
              </CollapsibleTrigger>
            </div>
          </div>
        </CardHeader>
        <CollapsibleContent>
          <CardContent className="pt-0">
            <ScrollArea className={cn("rounded-md border", isExpanded ? "h-64" : "h-32")}>
              {logs.length === 0 ? (
                <p className="p-4 text-sm text-muted-foreground text-center">
                  No activity yet. Start services to see logs.
                </p>
              ) : (
                <div className="p-2 space-y-1">
                  {[...logs].reverse().map((log, index) => (
                    <div
                      key={index}
                      className="flex items-start gap-2 p-2 rounded-md hover:bg-muted/50 text-sm"
                    >
                      {getLevelIcon(log.level)}
                      <span className="text-xs text-muted-foreground font-mono min-w-[70px]">
                        {formatTime(log.timestamp)}
                      </span>
                      {log.service && (
                        <Badge variant="outline" className="text-xs px-1.5 py-0">
                          {log.service}
                        </Badge>
                      )}
                      <span className={cn("flex-1", getLevelClass(log.level))}>
                        {log.message}
                      </span>
                    </div>
                  ))}
                </div>
              )}
            </ScrollArea>
          </CardContent>
        </CollapsibleContent>
      </Collapsible>
    </Card>
  );
}

function UsageExample() {
  const kvExample = `import { createClient } from '@temps-sdk/kv'

const kv = createClient()

await kv.set('user:1', { name: 'Alice' })
const user = await kv.get('user:1')`;

  const blobExample = `import { createClient } from '@temps-sdk/blob'

const blob = createClient()

await blob.put('images/logo.png', file)
const list = await blob.list({ prefix: 'images/' })`;

  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-lg">Quick Start</CardTitle>
        <CardDescription>
          Use the Temps SDK with LocalTemps in your applications
        </CardDescription>
      </CardHeader>
      <CardContent>
        <div className="grid gap-4 md:grid-cols-2">
          <div className="space-y-2">
            <div className="flex items-center justify-between">
              <h4 className="text-sm font-medium flex items-center gap-2">
                <Database className="h-4 w-4" />
                KV Storage
              </h4>
              <CopyButton value={kvExample} size="sm" variant="ghost" className="h-7" />
            </div>
            <pre className="p-3 rounded-lg bg-muted font-mono text-xs overflow-x-auto">
              {kvExample}
            </pre>
          </div>
          <div className="space-y-2">
            <div className="flex items-center justify-between">
              <h4 className="text-sm font-medium flex items-center gap-2">
                <HardDrive className="h-4 w-4" />
                Blob Storage
              </h4>
              <CopyButton value={blobExample} size="sm" variant="ghost" className="h-7" />
            </div>
            <pre className="p-3 rounded-lg bg-muted font-mono text-xs overflow-x-auto">
              {blobExample}
            </pre>
          </div>
        </div>
      </CardContent>
    </Card>
  );
}

function ServicesPage({
  services,
  envConfig,
  activityLogs,
  isStarting,
  isStopping,
  allRunning,
  anyRunning,
  error,
  onStartServices,
  onStopServices,
  onClearLogs,
  onDismissError,
}: {
  services: ServiceStatus[];
  envConfig: EnvConfig | null;
  activityLogs: ActivityLogEntry[];
  isStarting: boolean;
  isStopping: boolean;
  allRunning: boolean;
  anyRunning: boolean;
  error: string | null;
  onStartServices: () => void;
  onStopServices: () => void;
  onClearLogs: () => void;
  onDismissError: () => void;
}) {
  return (
    <div className="space-y-6">
      {/* Error Banner */}
      {error && (
        <Card className="border-destructive bg-destructive/10">
          <CardContent className="flex items-center justify-between p-4">
            <div className="flex items-center gap-2 text-destructive">
              <XCircle className="h-5 w-5" />
              <p className="text-sm">{error}</p>
            </div>
            <Button variant="ghost" size="sm" onClick={onDismissError}>
              Dismiss
            </Button>
          </CardContent>
        </Card>
      )}

      {/* Controls */}
      <div className="flex gap-3">
        <Button
          onClick={onStartServices}
          disabled={isStarting || allRunning}
          className="flex-1"
          variant={allRunning ? "secondary" : "default"}
        >
          {isStarting ? (
            <>
              <Loader2 className="h-4 w-4 animate-spin" />
              Starting...
            </>
          ) : (
            <>
              <Play className="h-4 w-4" />
              Start All Services
            </>
          )}
        </Button>
        <Button
          onClick={onStopServices}
          disabled={isStopping || !anyRunning}
          variant="outline"
          className="flex-1"
        >
          {isStopping ? (
            <>
              <Loader2 className="h-4 w-4 animate-spin" />
              Stopping...
            </>
          ) : (
            <>
              <Square className="h-4 w-4" />
              Stop All Services
            </>
          )}
        </Button>
      </div>

      {/* Services Grid */}
      <div className="grid gap-4 md:grid-cols-2">
        {services.map((service) => (
          <ServiceCard key={service.service_type} service={service} />
        ))}
      </div>

      {/* Environment Variables */}
      {envConfig && <EnvVarsSection config={envConfig} />}

      {/* Activity Log */}
      <ActivityLog logs={activityLogs} onClear={onClearLogs} />

      {/* Usage Examples */}
      <UsageExample />
    </div>
  );
}

function SettingsPage() {
  return (
    <div className="space-y-6">
      <Card>
        <CardHeader>
          <CardTitle>Settings</CardTitle>
          <CardDescription>
            Configure LocalTemps settings
          </CardDescription>
        </CardHeader>
        <CardContent>
          <p className="text-sm text-muted-foreground">
            Settings configuration coming soon.
          </p>
        </CardContent>
      </Card>
    </div>
  );
}

function App() {
  const [services, setServices] = useState<ServiceStatus[]>([]);
  const [envConfig, setEnvConfig] = useState<EnvConfig | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [isStarting, setIsStarting] = useState(false);
  const [isStopping, setIsStopping] = useState(false);
  const [apiRunning, setApiRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [activityLogs, setActivityLogs] = useState<ActivityLogEntry[]>([]);
  const [activePage, setActivePage] = useState<NavPage>('services');

  const fetchStatus = useCallback(async () => {
    try {
      const result = await invoke<CommandResult<ServiceStatus[]>>("get_services_status");
      if (result.success && result.data) {
        setServices(result.data);
      }

      const running = await invoke<boolean>("is_api_running");
      setApiRunning(running);

      const config = await invoke<EnvConfig>("get_env_config");
      setEnvConfig(config);

      const logs = await invoke<ActivityLogEntry[]>("get_activity_logs");
      setActivityLogs(logs);
    } catch (e) {
      console.error("Failed to fetch status:", e);
    } finally {
      setIsLoading(false);
    }
  }, []);

  const clearLogs = async () => {
    try {
      await invoke("clear_activity_logs");
      setActivityLogs([]);
    } catch (e) {
      console.error("Failed to clear logs:", e);
    }
  };

  useEffect(() => {
    fetchStatus();
    const interval = setInterval(fetchStatus, 5000);
    return () => clearInterval(interval);
  }, [fetchStatus]);

  const startServices = async () => {
    setIsStarting(true);
    setError(null);
    try {
      const result = await invoke<CommandResult<ServiceStatus[]>>("start_services");
      if (result.success && result.data) {
        setServices(result.data);
      } else if (result.error) {
        setError(result.error);
      }

      const apiResult = await invoke<CommandResult<string>>("start_api_server");
      if (apiResult.success) {
        setApiRunning(true);
      } else if (apiResult.error) {
        setError(apiResult.error);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setIsStarting(false);
      fetchStatus();
    }
  };

  const stopServices = async () => {
    setIsStopping(true);
    setError(null);
    try {
      const result = await invoke<CommandResult<void>>("stop_services");
      if (!result.success && result.error) {
        setError(result.error);
      }
      setApiRunning(false);
    } catch (e) {
      setError(String(e));
    } finally {
      setIsStopping(false);
      fetchStatus();
    }
  };

  const allRunning = services.length > 0 && services.every(s => s.running);
  const anyRunning = services.some(s => s.running);

  if (isLoading) {
    return (
      <main className="flex min-h-screen items-center justify-center bg-background">
        <div className="flex flex-col items-center gap-4">
          <Loader2 className="h-8 w-8 animate-spin text-primary" />
          <p className="text-muted-foreground">Loading LocalTemps...</p>
        </div>
      </main>
    );
  }

  const renderPage = () => {
    switch (activePage) {
      case 'services':
        return (
          <ServicesPage
            services={services}
            envConfig={envConfig}
            activityLogs={activityLogs}
            isStarting={isStarting}
            isStopping={isStopping}
            allRunning={allRunning}
            anyRunning={anyRunning}
            error={error}
            onStartServices={startServices}
            onStopServices={stopServices}
            onClearLogs={clearLogs}
            onDismissError={() => setError(null)}
          />
        );
      case 'analytics':
        return <AnalyticsInspector />;
      case 'settings':
        return <SettingsPage />;
      default:
        return null;
    }
  };

  const getPageTitle = () => {
    switch (activePage) {
      case 'services':
        return 'Services';
      case 'analytics':
        return 'Analytics Inspector';
      case 'settings':
        return 'Settings';
      default:
        return 'LocalTemps';
    }
  };

  return (
    <SidebarProvider>
      <AppSidebar
        activePage={activePage}
        onNavigate={setActivePage}
        apiRunning={apiRunning}
      />
      <SidebarInset>
        <header className="flex h-14 shrink-0 items-center gap-2 border-b px-4">
          <SidebarTrigger className="-ml-1" />
          <div className="flex-1">
            <h1 className="text-lg font-semibold">{getPageTitle()}</h1>
          </div>
        </header>
        <main className="flex-1 overflow-auto">
          <div className="container mx-auto max-w-4xl py-6 px-4">
            {renderPage()}
          </div>
        </main>
        <footer className="border-t px-4 py-3 text-center text-sm text-muted-foreground">
          LocalTemps provides local development services compatible with the Temps SDK.
        </footer>
      </SidebarInset>
    </SidebarProvider>
  );
}

export default App;
