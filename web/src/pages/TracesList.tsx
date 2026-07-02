import {
  EnvironmentResponse,
  ProjectResponse,
  SpanStatusCode,
  TraceSummary,
} from '@/api/client'
import {
  getEnvironmentsOptions,
  getProjectDeploymentsOptions,
  queryTraceSummariesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { CodeBlock } from '@/components/ui/code-block'
import { EmptyState } from '@/components/ui/empty-state'
import { Input } from '@/components/ui/input'
import { useDebounce } from '@/hooks/useDebounce'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  AlertTriangle,
  ArrowDown,
  ArrowUp,
  ArrowUpDown,
  Bot,
  Check,
  ChevronLeft,
  ChevronRight,
  Clock,
  Code2,
  FileCode,
  RefreshCw,
  Search,
  Settings2,
  Terminal,
  Workflow,
} from 'lucide-react'
import {
  ReactElement,
  useCallback,
  useEffect,
  useMemo,
  useState,
} from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'

interface TracesListProps {
  project: ProjectResponse
}

type TimeRange = '1h' | '6h' | '24h' | '7d' | '30d'

function statusBadge(status: SpanStatusCode) {
  switch (status) {
    case 'OK':
      return <Badge variant="success">OK</Badge>
    case 'ERROR':
      return <Badge variant="destructive">Error</Badge>
    default:
      return null
  }
}

function kindBadge(kind: string) {
  const colors: Record<string, string> = {
    SERVER: 'bg-blue-100 text-blue-800 dark:bg-blue-900/30 dark:text-blue-300',
    CLIENT:
      'bg-purple-100 text-purple-800 dark:bg-purple-900/30 dark:text-purple-300',
    PRODUCER:
      'bg-amber-100 text-amber-800 dark:bg-amber-900/30 dark:text-amber-300',
    CONSUMER:
      'bg-green-100 text-green-800 dark:bg-green-900/30 dark:text-green-300',
    INTERNAL:
      'bg-gray-100 text-gray-800 dark:bg-gray-900/30 dark:text-gray-300',
  }
  return (
    <span
      className={`inline-flex items-center rounded px-1.5 py-0.5 text-xs font-medium ${colors[kind] || colors.INTERNAL}`}
    >
      {kind}
    </span>
  )
}

function formatDuration(ms: number): string {
  if (ms < 1) return '<1ms'
  if (ms < 1000) return `${Math.round(ms)}ms`
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`
  return `${(ms / 60_000).toFixed(1)}m`
}

function durationColor(ms: number): string {
  if (ms < 100) return 'text-green-600 dark:text-green-400'
  if (ms < 500) return 'text-yellow-600 dark:text-yellow-400'
  if (ms < 2000) return 'text-orange-600 dark:text-orange-400'
  return 'text-red-600 dark:text-red-400'
}

const PAGE_SIZE = 10

// ── Setup Section ───────────────────────────────────────────────────

const NextJsIcon = () => (
  <svg viewBox="0 0 24 24" className="h-5 w-5" fill="currentColor">
    <path d="M11.572 0c-.176 0-.31.001-.358.007a19.76 19.76 0 0 1-.364.033C7.443.346 4.25 2.185 2.228 5.012a11.875 11.875 0 0 0-2.119 5.243c-.096.659-.108.854-.108 1.747s.012 1.089.108 1.748c.652 4.506 3.86 8.292 8.209 9.695.779.25 1.6.422 2.534.525.363.04 1.935.04 2.299 0 1.611-.178 2.977-.577 4.323-1.264.207-.106.247-.134.219-.158-.02-.013-.9-1.193-1.955-2.62l-1.919-2.592-2.404-3.558a338.739 338.739 0 0 0-2.422-3.556c-.009-.002-.018 1.579-.023 3.51-.007 3.38-.01 3.515-.052 3.595a.426.426 0 0 1-.206.214c-.075.037-.14.044-.495.044H7.81l-.108-.068a.438.438 0 0 1-.157-.171l-.05-.106.006-4.703.007-4.705.072-.092a.645.645 0 0 1 .174-.143c.096-.047.134-.051.54-.051.478 0 .558.018.682.154.035.038 1.337 1.999 2.895 4.361a10760.433 10760.433 0 0 0 4.735 7.17l1.9 2.879.096-.063a12.317 12.317 0 0 0 2.466-2.163 11.944 11.944 0 0 0 2.824-6.134c.096-.66.108-.854.108-1.748 0-.893-.012-1.088-.108-1.747-.652-4.506-3.859-8.292-8.208-9.695a12.597 12.597 0 0 0-2.499-.523A33.119 33.119 0 0 0 11.573 0zm4.069 7.217c.347 0 .408.005.486.047a.473.473 0 0 1 .237.277c.018.06.023 1.365.018 4.304l-.006 4.218-.744-1.14-.746-1.14v-3.066c0-1.982.01-3.097.023-3.15a.478.478 0 0 1 .233-.296c.096-.05.13-.054.5-.054z" />
  </svg>
)

const NodeIcon = () => (
  <svg viewBox="0 0 24 24" className="h-5 w-5" fill="currentColor">
    <path d="M11.998 24c-.321 0-.641-.084-.922-.247l-2.936-1.737c-.438-.245-.224-.332-.08-.383.585-.203.703-.25 1.328-.604.065-.037.151-.023.218.017l2.256 1.339a.286.286 0 00.272 0l8.795-5.076a.276.276 0 00.134-.238V7.12a.283.283 0 00-.137-.242L12.133 1.804a.281.281 0 00-.271 0L3.075 6.879a.284.284 0 00-.139.242v10.146c0 .097.052.189.139.235l2.409 1.392c1.307.654 2.108-.116 2.108-.89V7.865c0-.142.114-.253.256-.253h1.115c.139 0 .255.112.255.253v10.142c0 1.745-.95 2.745-2.604 2.745-.508 0-.909 0-2.026-.551L2.28 18.675a1.856 1.856 0 01-.922-1.607V6.922c0-.663.353-1.281.922-1.61L11.075.236a1.925 1.925 0 011.848 0L21.72 5.312c.57.329.924.947.924 1.61v10.146c0 .663-.354 1.278-.924 1.609l-8.797 5.076c-.28.163-.6.247-.925.247zm2.718-6.993c-3.848 0-4.656-1.766-4.656-3.248 0-.14.114-.253.255-.253h1.136c.127 0 .235.092.254.219.172 1.155.681 1.739 3.01 1.739 1.855 0 2.644-.42 2.644-1.404 0-.568-.224-.991-3.105-1.273-2.407-.239-3.897-.772-3.897-2.698 0-1.776 1.497-2.833 4.005-2.833 2.818 0 4.213.977 4.39 3.08a.26.26 0 01-.067.191.26.26 0 01-.184.081h-1.141c-.118 0-.222-.084-.247-.198-.274-1.215-.938-1.603-2.75-1.603-2.029 0-2.264.706-2.264 1.235 0 .641.278.828 3.008 1.19 2.701.357 3.99.863 3.99 2.762-.001 1.918-1.6 3.018-4.39 3.018z" />
  </svg>
)

const ExpressIcon = () => (
  <svg viewBox="0 0 24 24" className="h-5 w-5" fill="currentColor">
    <path d="M24 18.588a1.529 1.529 0 01-1.895-.72l-3.45-4.771-.5-.667-4.003 5.444a1.466 1.466 0 01-1.802.708l5.158-6.92-4.798-6.251a1.595 1.595 0 011.9.666l3.576 4.83 3.596-4.81a1.435 1.435 0 011.788-.668L21.708 7.9l-2.522 3.283a.666.666 0 000 .994l4.804 6.412zM.002 11.576l.42-2.075c1.154-4.103 5.858-5.81 9.094-3.27 1.895 1.489 2.368 3.597 2.275 5.973H1.116C.943 16.447 4.005 19.009 7.92 17.7a4.078 4.078 0 002.582-2.876c.207-.666.548-.78 1.174-.588a5.417 5.417 0 01-2.589 3.957 6.272 6.272 0 01-7.306-.933 6.575 6.575 0 01-1.64-3.858c0-.235-.08-.455-.134-.666A88.33 88.33 0 010 11.577zm1.127-.286h9.654c-.06-3.076-2.001-5.258-4.59-5.278-2.882-.04-4.944 2.094-5.071 5.264z" />
  </svg>
)

const FastifyIcon = () => (
  <svg viewBox="0 0 24 24" className="h-5 w-5" fill="currentColor">
    <path d="M10.183 5.47h5.947v.823c0 .355.287.64.642.64h.641v-.64h.641l-.641-.823h.32V3.826h-1.28v1h-.642v-.36a1.923 1.923 0 00-1.923-1.92h-1.904a1.923 1.923 0 00-1.72 1.076h-5.48l-.78-.693-.24.373.52.6-.12.247.54.48-.12.247.54.48-.12.247.6.54-.12.246.54.474v1.53z" />
  </svg>
)

const DenoIcon = () => (
  <svg viewBox="0 0 24 24" className="h-5 w-5" fill="currentColor">
    <path d="M12 0a12 12 0 1 0 0 24 12 12 0 0 0 0-24zm-.029 3.04c4.56 0 8.302 2.952 8.302 6.634 0 2.715-1.844 4.926-4.543 6.03l.824 2.978.01.048a.17.17 0 0 1-.13.2l-2.152.48-.049.009a.17.17 0 0 1-.178-.124l-.854-3.105c-.394.033-.79.033-1.184.033-.705 0-1.395-.054-2.056-.15l-.885 3.224a.17.17 0 0 1-.208.116l-2.15-.48-.045-.013a.17.17 0 0 1-.088-.232l.872-3.167C5.116 14.48 3.39 12.428 3.39 9.935c0-4.035 3.906-6.895 8.58-6.895zm.38 3.478a1.09 1.09 0 1 0 0 2.18 1.09 1.09 0 0 0 0-2.18z" />
  </svg>
)

const PythonIcon = () => (
  <svg viewBox="0 0 24 24" className="h-5 w-5" fill="currentColor">
    <path d="M14.25.18l.9.2.73.26.59.3.45.32.34.34.25.34.16.33.1.3.04.26.02.2-.01.13V8.5l-.05.63-.13.55-.21.46-.26.38-.3.31-.33.25-.35.19-.35.14-.33.1-.3.07-.26.04-.21.02H8.77l-.69.05-.59.14-.5.22-.41.27-.33.32-.27.35-.2.36-.15.37-.1.35-.07.32-.04.27-.02.21v3.06H3.17l-.21-.03-.28-.07-.32-.12-.35-.18-.36-.26-.36-.36-.35-.46-.32-.59-.28-.73-.21-.88-.14-1.05-.05-1.23.06-1.22.16-1.04.24-.87.32-.71.36-.57.4-.44.42-.33.42-.24.4-.16.36-.1.32-.05.24-.01h.16l.06.01h8.16v-.83H6.18l-.01-2.75-.02-.37.05-.34.11-.31.17-.28.25-.26.31-.23.38-.2.44-.18.51-.15.58-.12.64-.1.71-.06.77-.04.84-.02 1.27.05z" />
  </svg>
)

const RustIcon = () => (
  <svg viewBox="0 0 24 24" className="h-5 w-5" fill="currentColor">
    <path d="M23.835 11.703l-1.008-.623a13.813 13.813 0 0 0-.028-.281l.866-.807a.348.348 0 0 0-.115-.578l-1.109-.414a8.927 8.927 0 0 0-.087-.275l.69-.96a.346.346 0 0 0-.225-.544l-1.175-.191a9.913 9.913 0 0 0-.142-.253l.49-1.083a.347.347 0 0 0-.324-.49l-1.19.042a7.07 7.07 0 0 0-.19-.219l.272-1.163a.347.347 0 0 0-.407-.407l-1.163.272-.219-.19.041-1.19a.346.346 0 0 0-.49-.323l-1.083.49-.253-.142-.191-1.175a.347.347 0 0 0-.544-.225l-.96.69c-.092-.03-.184-.061-.276-.086L14.823.939a.348.348 0 0 0-.578-.115l-.807.866c-.093-.01-.187-.02-.28-.028L12.534.655a.346.346 0 0 0-.588 0l-.624 1.008-.28.028-.807-.866a.348.348 0 0 0-.578.115l-.414 1.109-.275.087-.96-.69a.346.346 0 0 0-.544.224l-.19 1.175-.253.143-1.083-.49a.347.347 0 0 0-.49.323l.041 1.19a7.16 7.16 0 0 0-.219.19L4.107 3.93a.347.347 0 0 0-.407.407l.272 1.164-.19.218-1.19-.041a.345.345 0 0 0-.324.49l.49 1.084c-.048.083-.095.167-.141.253l-1.175.19a.347.347 0 0 0-.225.544l.69.96a8.927 8.927 0 0 0-.087.275l-1.109.414a.347.347 0 0 0-.115.579l.866.806c-.01.094-.02.187-.028.281l-1.008.623a.347.347 0 0 0 0 .588l1.008.624c.008.094.017.187.028.28l-.866.807a.347.347 0 0 0 .115.578l1.109.414.087.275-.69.96a.346.346 0 0 0 .225.544l1.175.191c.046.086.093.17.141.253l-.49 1.083a.347.347 0 0 0 .324.49l1.19-.041c.072.074.144.147.218.218l-.272 1.164a.346.346 0 0 0 .407.407l1.164-.272.218.19-.041 1.19a.346.346 0 0 0 .49.323l1.083-.49.253.142.191 1.175a.346.346 0 0 0 .544.225l.96-.69.276.087.414 1.109a.346.346 0 0 0 .578.115l.807-.866.28.028.624 1.008a.346.346 0 0 0 .588 0l.624-1.008.28-.028.807.866a.347.347 0 0 0 .578-.115l.414-1.109.275-.087.96.69a.345.345 0 0 0 .544-.225l.191-1.175.253-.142 1.083.49a.347.347 0 0 0 .49-.323l-.041-1.19.218-.19 1.164.272a.346.346 0 0 0 .407-.407l-.272-1.164.19-.218 1.19.041a.347.347 0 0 0 .323-.49l-.49-1.083.143-.253 1.175-.191a.346.346 0 0 0 .225-.544l-.69-.96.087-.275 1.109-.414a.347.347 0 0 0 .115-.579l-.866-.806c.01-.093.02-.187.028-.28l1.008-.624a.347.347 0 0 0 0-.588z" />
  </svg>
)

type CodeLang =
  | 'bash'
  | 'yaml'
  | 'json'
  | 'javascript'
  | 'typescript'
  | 'shell'
  | 'text'
  | 'python'
  | 'go'

type ExtraBlock = {
  title: string
  fileName: string
  lang: CodeLang
  code: string
}

type FrameworkPreset = {
  id: string
  name: string
  description: string
  icon: () => ReactElement
  install: string
  installLang: CodeLang
  fileName: string
  setupLang: CodeLang
  setupCode: string
  extra?: ExtraBlock
}

const frameworkPresets: FrameworkPreset[] = [
  {
    id: 'nextjs',
    name: 'Next.js',
    description: 'App Router + Node runtime',
    icon: NextJsIcon,
    install: 'npm install @vercel/otel @opentelemetry/api',
    installLang: 'bash',
    fileName: 'instrumentation.ts',
    setupLang: 'typescript',
    setupCode: `// instrumentation.ts (project root)
import { registerOTel } from '@vercel/otel'

export function register() {
  registerOTel({ serviceName: '__SERVICE_NAME__' })
}`,
    extra: {
      title: 'Enable the instrumentation hook',
      fileName: 'next.config.js',
      lang: 'javascript',
      code: `// next.config.js
/** @type {import('next').NextConfig} */
module.exports = {
  experimental: { instrumentationHook: true },
}`,
    },
  },
  {
    id: 'node',
    name: 'Node.js',
    description: 'Plain Node, Hono, Koa',
    icon: NodeIcon,
    install: `npm install @opentelemetry/sdk-node @opentelemetry/api \\
  @opentelemetry/auto-instrumentations-node \\
  @opentelemetry/exporter-trace-otlp-proto`,
    installLang: 'bash',
    fileName: 'tracing.ts',
    setupLang: 'typescript',
    setupCode: `// tracing.ts — import this FIRST, before your app code.
import { NodeSDK } from '@opentelemetry/sdk-node'
import { OTLPTraceExporter } from '@opentelemetry/exporter-trace-otlp-proto'
import { getNodeAutoInstrumentations } from '@opentelemetry/auto-instrumentations-node'

const sdk = new NodeSDK({
  serviceName: '__SERVICE_NAME__',
  traceExporter: new OTLPTraceExporter(),
  instrumentations: [getNodeAutoInstrumentations()],
})

sdk.start()`,
    extra: {
      title: 'Start with tracing preloaded',
      fileName: 'start command',
      lang: 'bash',
      code: `# Load tracing before the app boots
node --require ./tracing.js ./dist/server.js`,
    },
  },
  {
    id: 'express',
    name: 'Express',
    description: 'Node.js + Express',
    icon: ExpressIcon,
    install: `npm install @opentelemetry/sdk-node \\
  @opentelemetry/instrumentation-express \\
  @opentelemetry/instrumentation-http \\
  @opentelemetry/exporter-trace-otlp-proto`,
    installLang: 'bash',
    fileName: 'tracing.ts',
    setupLang: 'typescript',
    setupCode: `// tracing.ts
import { NodeSDK } from '@opentelemetry/sdk-node'
import { HttpInstrumentation } from '@opentelemetry/instrumentation-http'
import { ExpressInstrumentation } from '@opentelemetry/instrumentation-express'
import { OTLPTraceExporter } from '@opentelemetry/exporter-trace-otlp-proto'

const sdk = new NodeSDK({
  serviceName: '__SERVICE_NAME__',
  traceExporter: new OTLPTraceExporter(),
  instrumentations: [new HttpInstrumentation(), new ExpressInstrumentation()],
})

sdk.start()`,
  },
  {
    id: 'fastify',
    name: 'Fastify',
    description: 'Node.js + Fastify',
    icon: FastifyIcon,
    install: `npm install @opentelemetry/sdk-node \\
  @opentelemetry/instrumentation-fastify \\
  @opentelemetry/instrumentation-http \\
  @opentelemetry/exporter-trace-otlp-proto`,
    installLang: 'bash',
    fileName: 'tracing.ts',
    setupLang: 'typescript',
    setupCode: `// tracing.ts
import { NodeSDK } from '@opentelemetry/sdk-node'
import { HttpInstrumentation } from '@opentelemetry/instrumentation-http'
import { FastifyInstrumentation } from '@opentelemetry/instrumentation-fastify'
import { OTLPTraceExporter } from '@opentelemetry/exporter-trace-otlp-proto'

const sdk = new NodeSDK({
  serviceName: '__SERVICE_NAME__',
  traceExporter: new OTLPTraceExporter(),
  instrumentations: [new HttpInstrumentation(), new FastifyInstrumentation()],
})

sdk.start()`,
  },
  {
    id: 'deno',
    name: 'Deno',
    description: 'Built-in OpenTelemetry',
    icon: DenoIcon,
    install: '# Deno 1.46+ ships with native OTel support — no install needed.',
    installLang: 'bash',
    fileName: 'main.ts',
    setupLang: 'typescript',
    setupCode: `// main.ts — run with:
//   OTEL_DENO=true \\
//   OTEL_SERVICE_NAME=__SERVICE_NAME__ \\
//   deno run --unstable-otel main.ts
//
// Deno auto-instruments fetch + Deno.serve and exports OTLP/http
// to OTEL_EXPORTER_OTLP_ENDPOINT.
Deno.serve((_req) => new Response('hello'))`,
  },
  {
    id: 'python',
    name: 'Python',
    description: 'FastAPI, Django, Flask',
    icon: PythonIcon,
    install: `pip install opentelemetry-distro opentelemetry-exporter-otlp
opentelemetry-bootstrap --action=install`,
    installLang: 'bash',
    fileName: 'start command',
    setupLang: 'bash',
    setupCode: `# Zero-code auto-instrumentation — no app changes needed.
OTEL_SERVICE_NAME=__SERVICE_NAME__ \\
OTEL_TRACES_EXPORTER=otlp \\
OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf \\
opentelemetry-instrument python main.py`,
  },
  {
    id: 'rust',
    name: 'Rust',
    description: 'Axum, Actix, Tokio',
    icon: RustIcon,
    install: `# Cargo.toml
opentelemetry = "0.24"
opentelemetry_sdk = { version = "0.24", features = ["rt-tokio"] }
opentelemetry-otlp = { version = "0.17", features = ["http-proto", "reqwest-client"] }
tracing = "0.1"
tracing-opentelemetry = "0.25"
tracing-subscriber = "0.3"`,
    installLang: 'text',
    fileName: 'src/main.rs',
    setupLang: 'text',
    setupCode: `use opentelemetry::KeyValue;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{runtime, Resource};
use tracing_subscriber::prelude::*;

fn init_tracing() {
    let provider = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .http()
                .with_protocol(opentelemetry_otlp::Protocol::HttpBinary),
        )
        .with_trace_config(
            opentelemetry_sdk::trace::Config::default().with_resource(Resource::new(vec![
                KeyValue::new("service.name", "__SERVICE_NAME__"),
            ])),
        )
        .install_batch(runtime::Tokio)
        .expect("init otlp");

    tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("app")))
        .with(tracing_subscriber::fmt::layer())
        .init();
}`,
  },
]

function OtelSetupSection({ project }: { project: ProjectResponse }) {
  const [selectedEnvId, setSelectedEnvId] = useState<string>('')
  const [selectedFrameworkId, setSelectedFrameworkId] = useState<string>('nextjs')

  const { data: environments } = useQuery({
    ...getEnvironmentsOptions({
      path: { project_id: project.id },
    }),
    enabled: !!project.id,
  })

  // Auto-select first environment
  useEffect(() => {
    if (environments && environments.length > 0 && !selectedEnvId) {
      setSelectedEnvId(String(environments[0].id))
    }
  }, [environments, selectedEnvId])

  const selectedEnv = useMemo(
    () => environments?.find((e) => String(e.id) === selectedEnvId),
    [environments, selectedEnvId]
  )

  const baseUrl = window.location.origin
  const otlpEndpoint = selectedEnv
    ? `${baseUrl}/api/otel/v1/${project.id}/${selectedEnv.id}/0`
    : `${baseUrl}/api/otel/v1/${project.id}/0/0`

  const preset =
    frameworkPresets.find((f) => f.id === selectedFrameworkId) ??
    frameworkPresets[0]

  const setupCode = preset.setupCode.split('__SERVICE_NAME__').join(project.name)

  const envVarsCode = `# Auto-injected on Temps deployments.
# Set these manually when running on Vercel, Fly, AWS, bare metal, etc.
OTEL_EXPORTER_OTLP_ENDPOINT=${otlpEndpoint}
OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf
OTEL_EXPORTER_OTLP_HEADERS=Authorization=Bearer <YOUR_API_KEY>
OTEL_SERVICE_NAME=${project.name}`

  return (
    <Card id="traces-setup">
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-base">
          <Code2 className="h-4 w-4" />
          Setup OpenTelemetry
        </CardTitle>
        <CardDescription>
          Pick your framework — the snippet and endpoint are pre-filled for{' '}
          <strong>{project.name}</strong>. Apps deployed on Temps get these env
          vars automatically.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-6">
        {/* Framework picker */}
        <div className="space-y-2">
          <h4 className="text-sm font-medium">Framework / runtime</h4>
          <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-4">
            {frameworkPresets.map((fw) => {
              const Icon = fw.icon
              const isSelected = selectedFrameworkId === fw.id
              return (
                <button
                  key={fw.id}
                  type="button"
                  onClick={() => setSelectedFrameworkId(fw.id)}
                  className={cn(
                    'flex items-center gap-3 rounded-lg border bg-card p-3 text-left transition-all hover:border-primary/60 hover:bg-accent/40',
                    isSelected &&
                      'border-primary bg-primary/5 ring-2 ring-primary/20'
                  )}
                  aria-pressed={isSelected}
                >
                  <div className="rounded-md bg-muted p-1.5 text-foreground shrink-0">
                    <Icon />
                  </div>
                  <div className="flex-1 min-w-0">
                    <p className="text-sm font-medium leading-none truncate">
                      {fw.name}
                    </p>
                    <p className="mt-1 text-[11px] text-muted-foreground truncate">
                      {fw.description}
                    </p>
                  </div>
                  {isSelected && (
                    <Check className="size-4 shrink-0 text-primary" />
                  )}
                </button>
              )
            })}
          </div>
        </div>

        {/* Deployed-on-Temps note */}
        <div className="rounded-md border border-green-200 bg-green-50 dark:border-green-900/50 dark:bg-green-900/10 p-3">
          <p className="text-xs text-green-800 dark:text-green-200">
            <strong>Deployed on Temps?</strong> The OTLP endpoint, auth token,
            service name, and version are injected automatically. Just install
            the SDK and add the instrumentation file below.
          </p>
        </div>

        {/* Step 1: Install */}
        <div className="space-y-2">
          <div className="flex items-center gap-2">
            <Terminal className="size-4 text-muted-foreground" />
            <h4 className="text-sm font-medium">1. Install dependencies</h4>
          </div>
          <CodeBlock code={preset.install} language={preset.installLang} />
        </div>

        {/* Step 2: Instrumentation file */}
        <div className="space-y-2">
          <div className="flex items-center gap-2">
            <FileCode className="size-4 text-muted-foreground" />
            <h4 className="text-sm font-medium">
              2. Create <code>{preset.fileName}</code>
            </h4>
          </div>
          <CodeBlock
            code={setupCode}
            language={preset.setupLang}
            title={preset.fileName}
          />
        </div>

        {/* Optional extra step */}
        {preset.extra && (
          <div className="space-y-2">
            <div className="flex items-center gap-2">
              <FileCode className="size-4 text-muted-foreground" />
              <h4 className="text-sm font-medium">3. {preset.extra.title}</h4>
            </div>
            <CodeBlock
              code={preset.extra.code}
              language={preset.extra.lang}
              title={preset.extra.fileName}
            />
          </div>
        )}

        {/* External hosting env vars */}
        <div className="space-y-2">
          <div className="flex items-center gap-2">
            <Settings2 className="size-4 text-muted-foreground" />
            <h4 className="text-sm font-medium">
              {preset.extra ? '4' : '3'}. External hosting — environment
              variables
            </h4>
          </div>
          <p className="text-xs text-muted-foreground">
            Skip this if you deploy on Temps. Required when running the app on
            Vercel, Fly, AWS, bare metal, etc.
          </p>
          <div className="flex items-center gap-3">
            <span className="text-xs text-muted-foreground shrink-0">
              Environment:
            </span>
            <Select value={selectedEnvId} onValueChange={setSelectedEnvId}>
              <SelectTrigger className="w-[200px] h-8">
                <SelectValue placeholder="Select environment" />
              </SelectTrigger>
              <SelectContent>
                {environments?.map((env: EnvironmentResponse) => (
                  <SelectItem key={env.id} value={String(env.id)}>
                    {env.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <CodeBlock code={envVarsCode} language="bash" title=".env" />
          <p className="text-xs text-muted-foreground">
            Replace <code>&lt;YOUR_API_KEY&gt;</code> with a Temps API key (
            <code>tk_...</code>) from{' '}
            <strong>Settings &rarr; API Keys</strong>.
          </p>
        </div>
      </CardContent>
    </Card>
  )
}

// A right-aligned, clickable column header that drives server-side sort.
// Shows a neutral up/down glyph when inactive, and the active direction arrow
// when this column is the sort key.
function SortHeader({
  label,
  active,
  order,
  onClick,
}: {
  label: string
  active: boolean
  order: 'asc' | 'desc'
  onClick: () => void
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-sort={active ? (order === 'asc' ? 'ascending' : 'descending') : 'none'}
      className="ml-auto inline-flex items-center gap-1 hover:text-foreground transition-colors"
    >
      {label}
      {active ? (
        order === 'asc' ? (
          <ArrowUp className="h-3.5 w-3.5" />
        ) : (
          <ArrowDown className="h-3.5 w-3.5" />
        )
      ) : (
        <ArrowUpDown className="h-3.5 w-3.5 opacity-40" />
      )}
    </button>
  )
}

// ── Main Component ──────────────────────────────────────────────────

export default function TracesList({ project }: TracesListProps) {
  const navigate = useNavigate()
  const [searchParams, setSearchParams] = useSearchParams()
  const { setBreadcrumbs } = useBreadcrumbs()
  usePageTitle(`Traces - ${project.name}`)

  // State from URL params
  const [timeRange, setTimeRange] = useState<TimeRange>(
    () => (searchParams.get('range') as TimeRange) || '24h'
  )
  const [serviceName, setServiceName] = useState(
    () => searchParams.get('service') || ''
  )
  const [status, setStatus] = useState(
    () => searchParams.get('status') || 'all'
  )
  const [search, setSearch] = useState(
    () => searchParams.get('q') || ''
  )
  const [namePattern, setNamePattern] = useState(
    () => searchParams.get('name') || ''
  )
  // Debounce the free-text filters so we don't fire a query on every keystroke.
  const debouncedSearch = useDebounce(search, 300)
  const debouncedNamePattern = useDebounce(namePattern, 300)
  const [environmentId, setEnvironmentId] = useState(
    () => searchParams.get('env') || 'all'
  )
  const [deploymentId, setDeploymentId] = useState(
    () => searchParams.get('deploy') || 'all'
  )
  const [page, setPage] = useState(() => {
    const p = searchParams.get('page')
    return p ? parseInt(p, 10) : 1
  })
  // Server-side sort. Default mirrors the backend: newest traces first.
  const [sortBy, setSortBy] = useState<'start_time' | 'duration'>(() =>
    searchParams.get('sort') === 'duration' ? 'duration' : 'start_time',
  )
  const [sortOrder, setSortOrder] = useState<'asc' | 'desc'>(() =>
    searchParams.get('dir') === 'asc' ? 'asc' : 'desc',
  )
  const [showSetup, setShowSetup] = useState(false)

  // Compute time window
  const { startTime, endTime } = useMemo(() => {
    const now = new Date()
    const start = new Date()
    switch (timeRange) {
      case '1h':
        start.setHours(start.getHours() - 1)
        break
      case '6h':
        start.setHours(start.getHours() - 6)
        break
      case '24h':
        start.setDate(start.getDate() - 1)
        break
      case '7d':
        start.setDate(start.getDate() - 7)
        break
      case '30d':
        start.setDate(start.getDate() - 30)
        break
    }
    return { startTime: start.toISOString(), endTime: now.toISOString() }
  }, [timeRange])

  // Fetch environments for the filter dropdown
  const { data: environments } = useQuery({
    ...getEnvironmentsOptions({
      path: { project_id: project.id },
    }),
    enabled: !!project.id,
  })

  // Fetch deployments for the selected environment (or all)
  const { data: deploymentsData } = useQuery({
    ...getProjectDeploymentsOptions({
      path: { id: project.id },
      query: {
        environment_id:
          environmentId !== 'all' ? Number(environmentId) : undefined,
        per_page: 50,
      },
    }),
    enabled: !!project.id,
  })

  const deployments = deploymentsData?.deployments

  // Sync state to URL
  useEffect(() => {
    const params = new URLSearchParams()
    if (timeRange !== '24h') params.set('range', timeRange)
    if (serviceName) params.set('service', serviceName)
    if (status !== 'all') params.set('status', status)
    if (debouncedSearch) params.set('q', debouncedSearch)
    if (debouncedNamePattern) params.set('name', debouncedNamePattern)
    if (environmentId !== 'all') params.set('env', environmentId)
    if (deploymentId !== 'all') params.set('deploy', deploymentId)
    if (page > 1) params.set('page', page.toString())
    if (sortBy !== 'start_time') params.set('sort', sortBy)
    if (sortOrder !== 'desc') params.set('dir', sortOrder)
    setSearchParams(params, { replace: true })
  }, [timeRange, serviceName, status, debouncedSearch, debouncedNamePattern, environmentId, deploymentId, page, sortBy, sortOrder, setSearchParams])

  // Toggle sort on a column header. Clicking the active column flips direction;
  // clicking a new column selects it (duration starts desc = slowest first,
  // timestamp starts desc = newest first — the most useful default each way).
  const handleSort = useCallback(
    (field: 'start_time' | 'duration') => {
      if (sortBy === field) {
        setSortOrder((d) => (d === 'desc' ? 'asc' : 'desc'))
      } else {
        setSortBy(field)
        setSortOrder('desc')
      }
      setPage(1)
    },
    [sortBy],
  )

  // Breadcrumbs
  useEffect(() => {
    setBreadcrumbs([
      { label: 'Projects', href: '/projects' },
      { label: project.name, href: `/projects/${project.slug}` },
      { label: 'Traces' },
    ])
  }, [project.name, project.slug, setBreadcrumbs])

  // Fetch trace summaries (one row per trace, server-side aggregation)
  const { data, isLoading, isFetching, refetch } = useQuery({
    ...queryTraceSummariesOptions({
      query: {
        project_id: project.id,
        start_time: startTime,
        end_time: endTime,
        service_name: serviceName || undefined,
        status: status !== 'all' ? status : undefined,
        trace_id: debouncedSearch || undefined,
        name_pattern: debouncedNamePattern || undefined,
        environment_id:
          environmentId !== 'all' ? Number(environmentId) : undefined,
        deployment_id:
          deploymentId !== 'all' ? Number(deploymentId) : undefined,
        sort_by: sortBy,
        sort_order: sortOrder,
        limit: PAGE_SIZE,
        offset: (page - 1) * PAGE_SIZE,
      },
    }),
  })

  const traces: TraceSummary[] = data?.data ?? []
  const totalCount = data?.total ?? 0
  const totalPages = Math.max(1, Math.ceil(totalCount / PAGE_SIZE))

  // "Has this project EVER received a trace?" — a window/filter-independent
  // probe (project_id only, limit 1). The main query above is scoped to the
  // selected time range and filters, so its `total` goes to 0 whenever the
  // window happens to be empty. Gating the setup onboarding on that would show
  // "set up OpenTelemetry" to a project with millions of historical traces just
  // because nothing landed in the last 24h. This probe answers the real
  // question the onboarding screen is for.
  const { data: anyTraceData, isLoading: isProbeLoading } = useQuery({
    ...queryTraceSummariesOptions({
      query: { project_id: project.id, limit: 1 },
    }),
    enabled: !!project.id,
  })
  const hasEverReceivedTraces = (anyTraceData?.total ?? 0) > 0

  const hasActiveFilters =
    !!search ||
    !!serviceName ||
    status !== 'all' ||
    environmentId !== 'all' ||
    deploymentId !== 'all'

  // Copy for the in-window empty state, in priority order:
  //  1. filters/window active → suggest adjusting them
  //  2. project has traces but none in this window → suggest widening the range
  //  3. project has never sent a trace → the genuine "get started" message
  const emptyStateDescription = hasActiveFilters
    ? 'Try adjusting your filters or time range.'
    : hasEverReceivedTraces
      ? 'No traces in the selected time range. Try widening the range.'
      : 'Traces will appear here once your application sends data via OpenTelemetry.'

  // Extract unique service names for the filter dropdown. Drop empty/missing
  // names: a trace whose root span never set `service.name` arrives as "", and
  // Radix's <SelectItem> throws ("must have a value prop that is not an empty
  // string") if one reaches the dropdown — crashing the whole page.
  const serviceNames = useMemo(() => {
    if (!traces.length) return []
    const names = new Set(
      traces.map((t) => t.service_name).filter((name): name is string => !!name)
    )
    return Array.from(names).sort()
  }, [traces])

  const handleTimeRangeChange = useCallback(
    (v: string) => {
      setTimeRange(v as TimeRange)
      setPage(1)
    },
    []
  )
  const handleStatusChange = useCallback(
    (v: string) => {
      setStatus(v)
      setPage(1)
    },
    []
  )
  const handleServiceChange = useCallback(
    (v: string) => {
      setServiceName(v === '__all__' ? '' : v)
      setPage(1)
    },
    []
  )
  const handleEnvironmentChange = useCallback(
    (v: string) => {
      setEnvironmentId(v)
      setDeploymentId('all') // Reset deployment when environment changes
      setPage(1)
    },
    []
  )
  const handleDeploymentChange = useCallback(
    (v: string) => {
      setDeploymentId(v)
      setPage(1)
    },
    []
  )

  return (
    <div className="space-y-4">
      {/* Header */}
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h2 className="text-2xl font-bold tracking-tight">Traces</h2>
          <p className="text-sm text-muted-foreground">
            Distributed traces from your application via OpenTelemetry
          </p>
        </div>
        <div className="flex items-center gap-3">
          <Button
            variant="ghost"
            size="icon"
            onClick={() => refetch()}
            disabled={isFetching}
          >
            <RefreshCw className={`h-4 w-4 ${isFetching ? 'animate-spin' : ''}`} />
          </Button>
          <Button
            variant="outline"
            size="sm"
            className="gap-2"
            onClick={() =>
              navigate(`/projects/${project.slug}/ai-gateway?tab=activity`)
            }
          >
            <Bot className="h-3.5 w-3.5" />
            <span className="hidden sm:inline">AI Traces</span>
          </Button>
          <Button
            variant="outline"
            size="sm"
            className="gap-2"
            onClick={() => {
              setShowSetup((v) => !v)
              requestAnimationFrame(() => {
                document
                  .getElementById('traces-setup')
                  ?.scrollIntoView({ behavior: 'smooth', block: 'start' })
              })
            }}
          >
            <Settings2 className="h-3.5 w-3.5" />
            <span className="hidden sm:inline">Setup</span>
          </Button>
          {totalCount > 0 && (
            <span className="text-sm text-muted-foreground">
              {totalCount.toLocaleString()} trace{totalCount !== 1 ? 's' : ''}
            </span>
          )}
        </div>
      </div>

      {/* Setup section — onboarding for a project that has NEVER received a
          trace, or when the user explicitly toggles it. Deliberately NOT gated
          on the windowed `totalCount`: an empty time range is "no results
          here", not "never set up", and must not resurface the setup wizard for
          a project with existing traces (see hasEverReceivedTraces probe). */}
      {((!isProbeLoading && !hasEverReceivedTraces) || showSetup) && (
        <OtelSetupSection project={project} />
      )}

      {/* Filters */}
      <Card>
        <CardContent className="p-3">
          <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap">
            <Select value={timeRange} onValueChange={handleTimeRangeChange}>
              <SelectTrigger className="w-full sm:w-[140px]">
                <Clock className="mr-2 h-3.5 w-3.5" />
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="1h">Last 1 hour</SelectItem>
                <SelectItem value="6h">Last 6 hours</SelectItem>
                <SelectItem value="24h">Last 24 hours</SelectItem>
                <SelectItem value="7d">Last 7 days</SelectItem>
                <SelectItem value="30d">Last 30 days</SelectItem>
              </SelectContent>
            </Select>

            <Select value={status} onValueChange={handleStatusChange}>
              <SelectTrigger className="w-full sm:w-[120px]">
                <SelectValue placeholder="Status" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All Status</SelectItem>
                <SelectItem value="OK">OK</SelectItem>
                <SelectItem value="ERROR">Error</SelectItem>
              </SelectContent>
            </Select>

            {environments && environments.length > 0 && (
              <Select
                value={environmentId}
                onValueChange={handleEnvironmentChange}
              >
                <SelectTrigger className="w-full sm:w-[180px]">
                  <SelectValue placeholder="Environment" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">All Environments</SelectItem>
                  {environments.map((env: EnvironmentResponse) => (
                    <SelectItem key={env.id} value={String(env.id)}>
                      {env.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            )}

            {deployments && deployments.length > 0 && (
              <Select
                value={deploymentId}
                onValueChange={handleDeploymentChange}
              >
                <SelectTrigger className="w-full sm:w-[180px]">
                  <SelectValue placeholder="Deployment" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">All Deployments</SelectItem>
                  {deployments.map((d) => (
                    <SelectItem key={d.id} value={String(d.id)}>
                      #{d.id}
                      {d.commit_hash ? ` (${d.commit_hash.slice(0, 7)})` : ''}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            )}

            {serviceNames.length > 0 && (
              <Select
                value={serviceName || '__all__'}
                onValueChange={handleServiceChange}
              >
                <SelectTrigger className="w-full sm:w-[180px]">
                  <SelectValue placeholder="Service" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="__all__">All Services</SelectItem>
                  {serviceNames.map((name) => (
                    <SelectItem key={name} value={name}>
                      {name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            )}

            <div className="relative flex-1 min-w-0 sm:min-w-[200px]">
              <Search className="absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
              <Input
                placeholder="Search by trace ID..."
                value={search}
                onChange={(e) => {
                  setSearch(e.target.value)
                  setPage(1)
                }}
                className="pl-8 h-9"
              />
            </div>

            <div className="relative flex-1 min-w-0 sm:min-w-[200px]">
              <Input
                placeholder="Filter by span name…"
                value={namePattern}
                onChange={(e) => {
                  setNamePattern(e.target.value)
                  setPage(1)
                }}
                className="h-9"
              />
            </div>
          </div>
        </CardContent>
      </Card>

      {/* Table */}
      {isLoading ? (
        <div className="space-y-2">
          {Array.from({ length: 8 }).map((_, i) => (
            <Skeleton key={`skel-${i}`} className="h-12 w-full" />
          ))}
        </div>
      ) : traces.length === 0 ? (
        <EmptyState
          icon={Workflow}
          title="No traces found"
          description={emptyStateDescription}
          action={
            // Only nudge toward setup for a project that has never sent a
            // trace — never when it just has nothing in the current window.
            !hasActiveFilters && !hasEverReceivedTraces ? (
              <Button
                variant="outline"
                size="sm"
                className="gap-2"
                onClick={() => {
                  document
                    .getElementById('traces-setup')
                    ?.scrollIntoView({ behavior: 'smooth', block: 'start' })
                }}
              >
                <Settings2 className="h-3.5 w-3.5" />
                Jump to setup
              </Button>
            ) : undefined
          }
        />
      ) : (
        <>
          <div className="rounded-md border overflow-x-auto">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead className="min-w-[200px] md:w-[300px]">Trace</TableHead>
                  <TableHead>Service</TableHead>
                  {environmentId === 'all' && <TableHead className="hidden lg:table-cell">Environment</TableHead>}
                  <TableHead className="hidden md:table-cell">Kind</TableHead>
                  <TableHead>Status</TableHead>
                  <TableHead className="text-right">
                    <SortHeader
                      label="Duration"
                      active={sortBy === 'duration'}
                      order={sortOrder}
                      onClick={() => handleSort('duration')}
                    />
                  </TableHead>
                  <TableHead className="hidden md:table-cell text-right">Spans</TableHead>
                  <TableHead className="hidden md:table-cell text-right">
                    <SortHeader
                      label="Timestamp"
                      active={sortBy === 'start_time'}
                      order={sortOrder}
                      onClick={() => handleSort('start_time')}
                    />
                  </TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {traces.map((trace) => (
                  <TableRow
                    key={trace.trace_id}
                    className="cursor-pointer hover:bg-muted/50"
                    onClick={() => navigate(trace.trace_id)}
                  >
                    <TableCell>
                      <div className="flex flex-col gap-0.5">
                        <span className="font-medium truncate max-w-[200px] md:max-w-[280px]">
                          {trace.root_span_name || (
                            <span className="text-muted-foreground italic">
                              (unnamed)
                            </span>
                          )}
                        </span>
                        <span className="text-xs text-muted-foreground font-mono truncate max-w-[200px] md:max-w-[280px]">
                          {trace.trace_id.slice(0, 16)}...
                        </span>
                      </div>
                    </TableCell>
                    <TableCell>
                      <span className="text-sm">
                        {trace.service_name || (
                          <span className="text-muted-foreground italic">
                            unknown
                          </span>
                        )}
                      </span>
                    </TableCell>
                    {environmentId === 'all' && (
                      <TableCell className="hidden lg:table-cell">
                        {trace.deployment_environment ? (
                          <Badge variant="secondary" className="font-normal">
                            {trace.deployment_environment}
                          </Badge>
                        ) : (
                          <span className="text-xs text-muted-foreground">—</span>
                        )}
                      </TableCell>
                    )}
                    <TableCell className="hidden md:table-cell">{kindBadge(trace.kind)}</TableCell>
                    <TableCell>
                      <div className="flex items-center gap-1.5">
                        {trace.error_count > 0
                          ? statusBadge('ERROR')
                          : statusBadge('OK')}
                        {trace.error_count > 0 && (
                          <span className="flex items-center text-xs text-destructive">
                            <AlertTriangle className="mr-0.5 h-3 w-3" />
                            {trace.error_count}
                          </span>
                        )}
                      </div>
                    </TableCell>
                    <TableCell className="text-right">
                      <span
                        className={`font-mono text-sm ${durationColor(trace.duration_ms)}`}
                      >
                        {formatDuration(trace.duration_ms)}
                      </span>
                    </TableCell>
                    <TableCell className="hidden md:table-cell text-right">
                      <Badge variant="outline" className="font-mono">
                        {trace.span_count}
                      </Badge>
                    </TableCell>
                    <TableCell className="hidden md:table-cell text-right text-sm text-muted-foreground">
                      {format(
                        new Date(trace.start_time),
                        'MMM d, HH:mm:ss.SSS'
                      )}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>

          {/* Pagination */}
          <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
            <div className="text-sm text-muted-foreground text-center sm:text-left">
              <span className="hidden sm:inline">
                Showing {(page - 1) * PAGE_SIZE + 1}–{Math.min(page * PAGE_SIZE, totalCount)} of{' '}
                {totalCount.toLocaleString()} trace{totalCount !== 1 ? 's' : ''}
              </span>
              <span className="sm:hidden">
                {totalCount.toLocaleString()} trace{totalCount !== 1 ? 's' : ''}
              </span>
            </div>
            <div className="flex items-center justify-center gap-1">
              <Button
                variant="outline"
                size="sm"
                onClick={() => setPage((p) => Math.max(1, p - 1))}
                disabled={page === 1}
              >
                <ChevronLeft className="h-4 w-4" />
              </Button>
              <span className="px-3 text-sm text-muted-foreground">
                {page} / {totalPages}
              </span>
              <Button
                variant="outline"
                size="sm"
                onClick={() => setPage((p) => p + 1)}
                disabled={page >= totalPages}
              >
                <ChevronRight className="h-4 w-4" />
              </Button>
            </div>
          </div>
        </>
      )}
    </div>
  )
}
