// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from "commander";
import { requireAuth } from "../../config/store.js";
import { setupClient, client, getErrorMessage } from "../../lib/api-client.js";
import {
  nodeDockerDiskUsageGet,
  nodeMetricsGetLatest,
  nodeMetricsGetRange,
} from "../../api/sdk.gen.js";
import type { DockerDiskUsage, MetricDataPoint } from "../../api/types.gen.js";
import { withSpinner } from "../../ui/spinner.js";
import { printTable, type TableColumn } from "../../ui/table.js";
import {
  newline,
  header,
  json,
  colors,
  keyValue,
  info,
} from "../../ui/output.js";

// ============================================================================
// Pure helpers (unit tested)
// ============================================================================

/** The control plane is always node 0 (`CONTROL_PLANE_NODE_ID`). */
export const CONTROL_PLANE_NODE_ID = 0;

export const SERVER_RANGES = ["1h", "6h", "24h", "7d"] as const;
export type ServerRange = (typeof SERVER_RANGES)[number];

/** Binary-unit byte formatter, e.g. `1.89 GiB`. */
export function formatBytes(bytes: number | null | undefined): string {
  if (bytes == null || !Number.isFinite(bytes)) return "-";
  const units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
  let v = Math.max(0, bytes);
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  const decimals = i === 0 ? 0 : v >= 100 ? 0 : v >= 10 ? 1 : 2;
  return `${v.toFixed(decimals)} ${units[i]}`;
}

export function formatPercent(v: number | null | undefined): string {
  if (v == null || !Number.isFinite(v)) return "-";
  return `${v.toFixed(1)}%`;
}

/**
 * Turn the per-bucket increases the range endpoint returns for `*_total`
 * counters into bytes/s, using the gap between buckets as the width (all
 * buckets in a range share one step; a lone point uses `fallbackStepSeconds`).
 */
export function toRatePerSecond(
  points: MetricDataPoint[],
  fallbackStepSeconds: number,
): MetricDataPoint[] {
  if (points.length === 0) return [];
  const times = points.map((p) => Date.parse(p.time));
  const stepFor = (i: number): number => {
    if (points.length === 1) return fallbackStepSeconds;
    const here = times[i] ?? NaN;
    const prev = times[i - 1];
    const next = times[i + 1];
    const gap =
      (i > 0 && prev != null ? here - prev : (next ?? NaN) - here) / 1000;
    return Number.isFinite(gap) && gap > 0 ? gap : fallbackStepSeconds;
  };
  return points.map((p, i) => ({
    time: p.time,
    value: Math.max(0, p.value) / stepFor(i),
  }));
}

/** Whether a metric name is a cumulative counter (mirrors `is_monotonic_counter`). */
export function isCumulativeCounter(metric: string): boolean {
  return (
    metric.endsWith("_total") ||
    metric.endsWith(".total") ||
    metric.endsWith("_count") ||
    metric.endsWith(".count")
  );
}

/** The rows of `temps server status`, derived from the latest snapshot. */
export type ServerStatusRow = { label: string; value: string };

export function buildStatusRows(
  latest: Record<string, number>,
): ServerStatusRow[] {
  const rows: ServerStatusRow[] = [];
  const has = (k: string) => latest[k] != null;

  if (has("node.cpu_percent")) {
    rows.push({
      label: "CPU",
      value: formatPercent(latest["node.cpu_percent"]),
    });
  }
  if (has("node.memory_used_bytes") && has("node.memory_total_bytes")) {
    rows.push({
      label: "Memory",
      value: `${formatBytes(latest["node.memory_used_bytes"])} / ${formatBytes(latest["node.memory_total_bytes"])} (${formatPercent(latest["node.memory_percent"])})`,
    });
  }
  if (has("node.disk_used_bytes") && has("node.disk_total_bytes")) {
    rows.push({
      label: "Disk",
      value: `${formatBytes(latest["node.disk_used_bytes"])} / ${formatBytes(latest["node.disk_total_bytes"])} (${formatPercent(latest["node.disk_percent"])})`,
    });
  }
  if (has("node.load_avg_1m")) {
    rows.push({
      label: "Load avg",
      value: `${(latest["node.load_avg_1m"] ?? 0).toFixed(2)} / ${(latest["node.load_avg_5m"] ?? 0).toFixed(2)} / ${(latest["node.load_avg_15m"] ?? 0).toFixed(2)}`,
    });
  }
  if (has("node.disk_read_bytes_total") || has("node.disk_write_bytes_total")) {
    rows.push({
      label: "Block I/O since boot",
      value: `read ${formatBytes(latest["node.disk_read_bytes_total"])} / write ${formatBytes(latest["node.disk_write_bytes_total"])}`,
    });
  }
  if (
    has("node.network_rx_bytes_total") ||
    has("node.network_tx_bytes_total")
  ) {
    rows.push({
      label: "Network since boot",
      value: `in ${formatBytes(latest["node.network_rx_bytes_total"])} / out ${formatBytes(latest["node.network_tx_bytes_total"])}`,
    });
  }
  if (has("node.fd_percent")) {
    rows.push({
      label: "File descriptors",
      value: `${latest["node.fd_allocated"] ?? 0} / ${latest["node.fd_max"] ?? 0} (${formatPercent(latest["node.fd_percent"])})`,
    });
  }
  return rows;
}

// ============================================================================
// Commander wiring
// ============================================================================

export function registerServerCommands(program: Command): void {
  const server = program
    .command("server")
    .description(
      "Resource usage of the machine running the Temps control plane (what /monitoring/server shows)",
    );

  server
    .command("status")
    .description(
      "Latest CPU, memory, disk, block I/O and network I/O sample for the control-plane host",
    )
    .option("--node <id>", "Node ID (0 = control plane)", "0")
    .option("--json", "Output in JSON format")
    .action(serverStatusAction);

  server
    .command("metrics")
    .description(
      "Time series of one control-plane host metric, e.g. node.cpu_percent or node.network_rx_bytes_total",
    )
    .requiredOption(
      "--metric <name>",
      "Metric name (node.cpu_percent, node.memory_used_bytes, node.disk_used_bytes, node.disk_read_bytes_total, node.network_tx_bytes_total, ...)",
    )
    .option("--range <range>", `Time window: ${SERVER_RANGES.join(", ")}`, "1h")
    .option("--node <id>", "Node ID (0 = control plane)", "0")
    .option("--json", "Output in JSON format")
    .action(serverMetricsAction);

  server
    .command("docker-disk-usage")
    .alias("df")
    .description(
      "Docker disk usage by images, containers, volumes and build cache (docker system df) on the control-plane host",
    )
    .option("--node <id>", "Node ID (0 = control plane)", "0")
    .option("--json", "Output in JSON format")
    .action(serverDockerDiskUsageAction);
}

// ============================================================================
// Actions
// ============================================================================

/** Parse `--node`: the whole option must be a non-negative integer, so `1abc` and `1.5` are rejected rather than read as node 1. */
export function parseNodeId(raw: string | undefined): number {
  const text = (raw ?? "0").trim();
  if (!/^\d+$/.test(text)) {
    throw new Error(
      `Invalid --node "${raw}": expected a non-negative integer (0 = control plane)`,
    );
  }
  return Number.parseInt(text, 10);
}

async function serverStatusAction(options: {
  node?: string;
  json?: boolean;
}): Promise<void> {
  await requireAuth();
  await setupClient();
  const nodeId = parseNodeId(options.node);

  const latest = await withSpinner(
    "Fetching latest server metrics...",
    async () => {
      const { data, error } = await nodeMetricsGetLatest({
        client,
        path: { id: nodeId },
      });
      if (error || !data) throw new Error(getErrorMessage(error));
      return data as Record<string, number>;
    },
  );

  if (options.json) {
    json({ node_id: nodeId, latest });
    return;
  }

  const rows = buildStatusRows(latest);
  newline();
  header(
    `Server status — node ${nodeId}${nodeId === CONTROL_PLANE_NODE_ID ? " (control plane)" : ""}`,
  );
  if (rows.length === 0) {
    newline();
    info(
      "No node samples yet. The first sample lands within one scrape interval (30s by default) once metric collection is enabled.",
    );
    newline();
    return;
  }
  for (const row of rows) keyValue(row.label, row.value);
  newline();
}

async function serverMetricsAction(options: {
  metric: string;
  range?: string;
  node?: string;
  json?: boolean;
}): Promise<void> {
  await requireAuth();
  await setupClient();
  const nodeId = parseNodeId(options.node);
  const range = options.range ?? "1h";
  if (!(SERVER_RANGES as readonly string[]).includes(range)) {
    throw new Error(
      `Invalid --range "${range}": expected one of ${SERVER_RANGES.join(", ")}`,
    );
  }

  const points = await withSpinner(
    `Fetching ${options.metric}...`,
    async () => {
      const { data, error } = await nodeMetricsGetRange({
        client,
        path: { id: nodeId },
        query: { metric: options.metric, range },
      });
      if (error || !data) throw new Error(getErrorMessage(error));
      return data;
    },
  );

  const cumulative = isCumulativeCounter(options.metric);
  const series = cumulative ? toRatePerSecond(points, 30) : points;

  if (options.json) {
    json({
      node_id: nodeId,
      metric: options.metric,
      range,
      unit: cumulative ? "per_second" : "value",
      points: series,
    });
    return;
  }

  newline();
  header(`${options.metric} — last ${range} on node ${nodeId}`);
  if (series.length === 0) {
    newline();
    info("No data points in this window.");
    newline();
    return;
  }
  const isBytes = options.metric.includes("bytes");
  const isPercent = options.metric.endsWith("_percent");
  const fmt = (v: number) =>
    isBytes
      ? `${formatBytes(v)}${cumulative ? "/s" : ""}`
      : isPercent
        ? formatPercent(v)
        : v.toFixed(2);
  const columns: TableColumn<MetricDataPoint>[] = [
    { header: "Time", accessor: (p) => new Date(p.time).toLocaleString() },
    { header: cumulative ? "Rate" : "Value", accessor: (p) => fmt(p.value) },
  ];
  printTable(series, columns, { style: "minimal" });
  newline();
}

async function serverDockerDiskUsageAction(options: {
  node?: string;
  json?: boolean;
}): Promise<void> {
  await requireAuth();
  await setupClient();
  const nodeId = parseNodeId(options.node);

  const usage = await withSpinner(
    "Running docker system df (this walks every image layer and volume)...",
    async () => {
      const { data, error } = await nodeDockerDiskUsageGet({
        client,
        path: { node_id: nodeId },
      });
      if (error || !data) throw new Error(getErrorMessage(error));
      return data;
    },
  );

  if (options.json) {
    json(usage);
    return;
  }

  type Row = { name: string; cat: DockerDiskUsage["images"] };
  const rows: Row[] = [
    { name: "Images", cat: usage.images },
    { name: "Containers", cat: usage.containers },
    { name: "Volumes", cat: usage.volumes },
    { name: "Build cache", cat: usage.build_cache },
  ];
  const columns: TableColumn<Row>[] = [
    { header: "Type", key: "name", color: (v) => colors.bold(v) },
    { header: "Total", accessor: (r) => r.cat.total_count.toString() },
    { header: "Active", accessor: (r) => r.cat.active_count.toString() },
    { header: "Size", accessor: (r) => formatBytes(r.cat.size_bytes) },
    {
      header: "Reclaimable",
      accessor: (r) =>
        r.cat.reclaimable_bytes == null
          ? "-"
          : formatBytes(r.cat.reclaimable_bytes),
    },
  ];

  newline();
  header(`Docker disk usage — node ${nodeId}`);
  printTable(rows, columns, { style: "minimal" });
  newline();
  keyValue("Total", formatBytes(usage.total_bytes));
  keyValue("Collected at", usage.collected_at);
  if (usage.api_version) keyValue("Docker API", usage.api_version);
  newline();
}
