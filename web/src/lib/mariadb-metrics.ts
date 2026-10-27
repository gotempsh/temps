// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Metrics the MariaDB collector reports (see
 *  `crates/temps-metrics/src/collector/mariadb.rs`), grouped for display. */
export type MetricGroup = {
  title: string
  metrics: string[]
}

/** Instance-wide MariaDB metrics. Per-schema metrics live in
 *  {@link MARIADB_PER_DATABASE_GROUPS} so a per-schema sum never sits
 *  unlabelled next to a true instance metric. */
export const MARIADB_METRIC_GROUPS: MetricGroup[] = [
  {
    title: 'Connections',
    metrics: [
      'mariadb.connections',
      'mariadb.max_connections',
      'mariadb.connections_used_ratio',
      'mariadb.connections_active',
      'mariadb.connections_idle',
      'mariadb.max_used_connections',
      'mariadb.threads_running',
      'mariadb.threads_cached',
      'mariadb.connections_created_total',
      'mariadb.threads_created_total',
      'mariadb.aborted_connects_total',
      'mariadb.aborted_clients_total',
      'mariadb.access_denied_errors_total',
      'mariadb.connections_refused_total',
    ],
  },
  {
    title: 'Queries',
    metrics: [
      'mariadb.queries_total',
      'mariadb.statements_select_total',
      'mariadb.statements_insert_total',
      'mariadb.statements_update_total',
      'mariadb.statements_delete_total',
      'mariadb.slow_queries_total',
      'mariadb.queries_long_running',
      'mariadb.oldest_query_seconds',
      'mariadb.statements_timed_out_total',
      'mariadb.full_scans_total',
      'mariadb.full_joins_total',
      'mariadb.tmp_tables_total',
      'mariadb.tmp_disk_tables_total',
      'mariadb.sort_merge_passes_total',
    ],
  },
  {
    title: 'Transactions & Locks',
    metrics: [
      'mariadb.commits_total',
      'mariadb.rollbacks_total',
      'mariadb.transactions_open',
      'mariadb.oldest_transaction_seconds',
      'mariadb.history_list_length',
      'mariadb.deadlocks_total',
      'mariadb.row_lock_waits_total',
      'mariadb.row_lock_waits_current',
      'mariadb.row_lock_wait_ms_total',
      'mariadb.table_locks_waited_total',
    ],
  },
  {
    title: 'Rows',
    metrics: [
      'mariadb.rows_read_total',
      'mariadb.rows_inserted_total',
      'mariadb.rows_updated_total',
      'mariadb.rows_deleted_total',
    ],
  },
  {
    title: 'Buffer Pool',
    metrics: [
      'mariadb.buffer_pool_hit_ratio',
      'mariadb.buffer_pool_size_bytes',
      'mariadb.buffer_pool_data_bytes',
      'mariadb.buffer_pool_usage_ratio',
      'mariadb.buffer_pool_dirty_bytes',
      'mariadb.buffer_pool_dirty_ratio',
      'mariadb.buffer_pool_wait_free_total',
    ],
  },
  {
    title: 'Disk I/O & Redo Log',
    metrics: [
      'mariadb.innodb_data_read_bytes_total',
      'mariadb.innodb_data_written_bytes_total',
      'mariadb.redo_log_written_bytes_total',
      'mariadb.redo_log_size_bytes',
      'mariadb.checkpoint_age_bytes',
      'mariadb.checkpoint_age_ratio',
      'mariadb.redo_log_waits_total',
      'mariadb.binlog_written_bytes_total',
    ],
  },
  {
    title: 'Network',
    metrics: [
      'mariadb.network_received_bytes_total',
      'mariadb.network_sent_bytes_total',
    ],
  },
  {
    title: 'Server',
    metrics: [
      'mariadb.uptime_seconds',
      'mariadb.memory_used_bytes',
      'mariadb.open_tables',
      'mariadb.table_open_cache',
      'mariadb.table_open_cache_usage_ratio',
      'mariadb.tables_opened_total',
      'mariadb.open_files',
      'mariadb.open_files_limit',
      'mariadb.open_files_usage_ratio',
    ],
  },
  {
    title: 'Replication',
    metrics: [
      'mariadb.replication_lag_seconds',
      'mariadb.replica_io_running',
      'mariadb.replica_sql_running',
    ],
  },
]

/** Metrics reported once per schema (label `schema`) plus an instance total.
 *  Must stay in sync with `MARIADB_PER_DATABASE_METRICS` in
 *  `crates/temps-providers/src/handlers/metrics_handlers.rs`. */
export const MARIADB_PER_DATABASE_GROUPS: MetricGroup[] = [
  {
    title: 'Storage',
    metrics: [
      'mariadb.database_size_bytes',
      'mariadb.data_size_bytes',
      'mariadb.index_size_bytes',
      'mariadb.data_free_bytes',
    ],
  },
  {
    title: 'Contents',
    metrics: ['mariadb.tables', 'mariadb.rows_estimated'],
  },
]

/** Every MariaDB metric, instance-wide and per schema. */
export const MARIADB_METRICS: string[] = [
  ...MARIADB_METRIC_GROUPS,
  ...MARIADB_PER_DATABASE_GROUPS,
].flatMap((group) => group.metrics)

/** 1/0 flags, shown as Yes/No rather than as a number. */
export const MARIADB_FLAG_METRICS: ReadonlySet<string> = new Set([
  'mariadb.replica_io_running',
  'mariadb.replica_sql_running',
])

/** Display names where the generic `snake_case` → Title Case is unclear. */
export const MARIADB_METRIC_LABELS: Record<string, string> = {
  'mariadb.connections': 'Connections',
  'mariadb.connections_used_ratio': 'Connections Used',
  'mariadb.connections_created_total': 'New Connections',
  'mariadb.connections_refused_total': 'Refused (Max Connections)',
  'mariadb.queries_total': 'Queries',
  'mariadb.statements_select_total': 'SELECT',
  'mariadb.statements_insert_total': 'INSERT',
  'mariadb.statements_update_total': 'UPDATE',
  'mariadb.statements_delete_total': 'DELETE',
  'mariadb.queries_long_running': 'Long-running (>30s)',
  'mariadb.statements_timed_out_total': 'Timed Out (max_statement_time)',
  'mariadb.full_scans_total': 'Full Table Scans',
  'mariadb.full_joins_total': 'Joins Without Index',
  'mariadb.tmp_tables_total': 'Temp Tables',
  'mariadb.tmp_disk_tables_total': 'Temp Tables on Disk',
  'mariadb.history_list_length': 'Undo History Length',
  'mariadb.row_lock_wait_ms_total': 'Row Lock Wait Time (ms)',
  'mariadb.innodb_data_read_bytes_total': 'Data Read',
  'mariadb.innodb_data_written_bytes_total': 'Data Written',
  'mariadb.redo_log_written_bytes_total': 'Redo Log Written',
  'mariadb.binlog_written_bytes_total': 'Binlog Written',
  'mariadb.network_received_bytes_total': 'Received',
  'mariadb.network_sent_bytes_total': 'Sent',
  'mariadb.database_size_bytes': 'Database Size',
  'mariadb.data_free_bytes': 'Free Space in Tablespaces',
  'mariadb.rows_estimated': 'Rows (estimated)',
  'mariadb.replica_io_running': 'Replica I/O Thread',
  'mariadb.replica_sql_running': 'Replica SQL Thread',
}
