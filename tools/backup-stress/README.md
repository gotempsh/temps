# Cloud backup stress benchmark

This opt-in harness creates a disposable PostgreSQL database with exact
100-million and 200-million-row checkpoints, streams WAL-G base backups into
RustFS, restores each checkpoint into a fresh PostgreSQL volume, and verifies
the exact restored row range.

It is intentionally not part of normal CI. The full run needs roughly 250 GB
of temporary Docker storage and can run for hours on laptop storage.

```bash
TEMPS_BACKUP_BENCH_ACK=1 ./tools/backup-stress/run.sh
```

Useful overrides:

```bash
TEMPS_BACKUP_BENCH_ACK=1 \
TEMPS_BENCH_FIRST_ROWS=1000000 \
TEMPS_BENCH_FINAL_ROWS=2000000 \
TEMPS_BENCH_BATCH_ROWS=250000 \
./tools/backup-stress/run.sh
```

If a run is interrupted after the first checkpoint, preserve the PostgreSQL
volume and resume at the final checkpoint with
`TEMPS_BENCH_SKIP_FIRST=1`. The final row target remains idempotent.

Reports are written outside the repository under `/tmp/temps-backup-bench-*`.
The report records database size, object-store growth while PostgreSQL is
still running, elapsed backup/restore time, peak `/tmp` and `/var/tmp` use in
the database container, and exact restored rows.

The harness pins RustFS `1.0.0-beta.9`. The earlier `alpha.98` image marked
its only disk offline during a 47.8 GB WAL-G stream despite ample free space;
the benchmark caught that failure before restore testing. Keep the pin until a
newer release has passed this same full-scale run.

## What “resumable” means

WAL-G streams PostgreSQL into an S3-compatible repository without a
database-sized host staging file. An interrupted `backup-push` itself starts a
new base-backup attempt. The OSS-to-Cloud mirror resumes at WAL-G object
boundaries: Cloud records every completed repository object, and a retry with
the same deterministic backup ID receives `upload_required=false` for those
objects. Completed objects are not sent twice.

The backup executor retries transient engine failures after 30, 60, and 120
seconds. The Cloud mirror uses a 30-second exponential backoff capped at 15
minutes. Permanent configuration failures are not retried.

Cleanup is explicit because the volumes are deliberately large:

```bash
./tools/backup-stress/cleanup.sh
```

## Deterministic network-failure checks

The large-data harness proves the storage path. The repeatable failure cases
run separately so CI does not wait real minutes:

```bash
cargo test --lib -p temps-backup-core executor::tests
cargo test --lib -p temps-cloud-client \
  backup_upload_recovers_each_network_boundary_without_changing_identity
cargo test --lib -p temps-cloud backup_mirror::tests
```

These inject transient engine failures, interrupt a live HTTP request after
its first body chunk, lose target/completion responses, and verify bounded
Cloud-mirror backoff. The Cloud backend integration suite separately verifies
that reconnecting a partially uploaded native snapshot skips committed
objects and requests upload only for missing objects.
