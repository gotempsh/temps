import { ProjectResponse } from '@/api/client'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import {
  AlertTriangle,
  Box,
  Clock,
  Cpu,
  MemoryStick,
  Server,
} from 'lucide-react'
import {
  Bar,
  BarChart,
  CartesianGrid,
  Line,
  LineChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts'

const performanceData = [
  { time: '00:00', latency: 120, cpu: 45, memory: 60 },
  { time: '01:00', latency: 132, cpu: 49, memory: 62 },
  { time: '02:00', latency: 125, cpu: 43, memory: 58 },
  { time: '03:00', latency: 130, cpu: 47, memory: 63 },
  { time: '04:00', latency: 135, cpu: 52, memory: 65 },
  { time: '05:00', latency: 145, cpu: 55, memory: 68 },
  { time: '06:00', latency: 150, cpu: 58, memory: 70 },
  { time: '07:00', latency: 155, cpu: 61, memory: 72 },
  { time: '08:00', latency: 160, cpu: 65, memory: 75 },
  { time: '09:00', latency: 165, cpu: 68, memory: 78 },
  { time: '10:00', latency: 170, cpu: 71, memory: 80 },
  { time: '11:00', latency: 175, cpu: 73, memory: 82 },
]

const errorData = [
  { time: '00:00', count: 2 },
  { time: '01:00', count: 1 },
  { time: '02:00', count: 3 },
  { time: '03:00', count: 0 },
  { time: '04:00', count: 1 },
  { time: '05:00', count: 4 },
  { time: '06:00', count: 2 },
  { time: '07:00', count: 1 },
  { time: '08:00', count: 0 },
  { time: '09:00', count: 2 },
  { time: '10:00', count: 3 },
  { time: '11:00', count: 1 },
]

interface MetricCardProps {
  title: string
  value: string
  description: string
  icon: React.ReactNode
  trend?: {
    value: number
    isPositive: boolean
  }
}

function MetricCard({
  title,
  value,
  description,
  icon,
  trend,
}: MetricCardProps) {
  return (
    <Card>
      <CardContent className="p-6">
        <div className="flex items-center justify-between">
          <div className="space-y-1">
            <p className="text-sm font-medium text-muted-foreground">{title}</p>
            <div className="flex items-baseline gap-2">
              <p className="text-2xl font-bold">{value}</p>
              {trend && (
                <span
                  className={
                    trend.isPositive ? 'text-green-600' : 'text-red-600'
                  }
                >
                  {trend.isPositive ? '+' : '-'}
                  {Math.abs(trend.value)}%
                </span>
              )}
            </div>
          </div>
          <div className="size-9 rounded-lg bg-primary/10 flex items-center justify-center">
            {icon}
          </div>
        </div>
        <p className="mt-2 text-xs text-muted-foreground">{description}</p>
      </CardContent>
    </Card>
  )
}

export function ProjectDeploymentMetrics({
  project,
}: {
  project: ProjectResponse
}) {
  return (
    <div className="space-y-6">
      <div className="grid gap-6 md:grid-cols-2 lg:grid-cols-4">
        <MetricCard
          title="Average Response Time"
          value="145ms"
          description="Last 24 hours average"
          icon={<Clock className="size-5 text-primary" />}
          trend={{ value: 12, isPositive: false }}
        />
        <MetricCard
          title="Error Rate"
          value="0.02%"
          description="Percentage of failed requests"
          icon={<AlertTriangle className="size-5 text-primary" />}
          trend={{ value: 5, isPositive: true }}
        />
        <MetricCard
          title="CPU Usage"
          value="65%"
          description="Current CPU utilization"
          icon={<Cpu className="size-5 text-primary" />}
          trend={{ value: 8, isPositive: false }}
        />
        <MetricCard
          title="Memory Usage"
          value="2.1GB"
          description="Current memory consumption"
          icon={<MemoryStick className="size-5 text-primary" />}
          trend={{ value: 3, isPositive: false }}
        />
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Performance Metrics</CardTitle>
          <CardDescription>System performance over time</CardDescription>
        </CardHeader>
        <CardContent>
          <Tabs defaultValue="latency">
            <TabsList>
              <TabsTrigger value="latency">Latency</TabsTrigger>
              <TabsTrigger value="resources">Resources</TabsTrigger>
              <TabsTrigger value="errors">Errors</TabsTrigger>
            </TabsList>
            <TabsContent value="latency" className="h-[300px] mt-4">
              <ResponsiveContainer width="100%" height="100%">
                <LineChart data={performanceData}>
                  <CartesianGrid strokeDasharray="3 3" />
                  <XAxis dataKey="time" />
                  <YAxis />
                  <Tooltip />
                  <Line
                    type="monotone"
                    dataKey="latency"
                    stroke="hsl(var(--primary))"
                  />
                </LineChart>
              </ResponsiveContainer>
            </TabsContent>
            <TabsContent value="resources" className="h-[300px] mt-4">
              <ResponsiveContainer width="100%" height="100%">
                <LineChart data={performanceData}>
                  <CartesianGrid strokeDasharray="3 3" />
                  <XAxis dataKey="time" />
                  <YAxis />
                  <Tooltip />
                  <Line
                    type="monotone"
                    dataKey="cpu"
                    stroke="hsl(var(--primary))"
                  />
                  <Line
                    type="monotone"
                    dataKey="memory"
                    stroke="hsl(var(--destructive))"
                  />
                </LineChart>
              </ResponsiveContainer>
            </TabsContent>
            <TabsContent value="errors" className="h-[300px] mt-4">
              <ResponsiveContainer width="100%" height="100%">
                <BarChart data={errorData}>
                  <CartesianGrid strokeDasharray="3 3" />
                  <XAxis dataKey="time" />
                  <YAxis />
                  <Tooltip />
                  <Bar dataKey="count" fill="hsl(var(--destructive))" />
                </BarChart>
              </ResponsiveContainer>
            </TabsContent>
          </Tabs>
        </CardContent>
      </Card>

      <div className="grid gap-6 md:grid-cols-2">
        <Card>
          <CardHeader>
            <div className="flex items-center justify-between">
              <div>
                <CardTitle>System Logs</CardTitle>
                <CardDescription>Recent system events</CardDescription>
              </div>
              <Server className="size-4 text-muted-foreground" />
            </div>
          </CardHeader>
          <CardContent className="space-y-4">
            {/* Add system logs here */}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <div className="flex items-center justify-between">
              <div>
                <CardTitle>Resource Usage</CardTitle>
                <CardDescription>Current system resources</CardDescription>
              </div>
              <Box className="size-4 text-muted-foreground" />
            </div>
          </CardHeader>
          <CardContent className="space-y-4">
            {/* Add resource usage details here */}
          </CardContent>
        </Card>
      </div>
    </div>
  )
}
