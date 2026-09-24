# Issue #1095: Preserve cluster CA on settings saves

Issue: https://github.com/gotempsh/temps/issues/1095
PR: https://github.com/gotempsh/temps/pull/1115
Branch: `fix/1095-preserve-settings-ca`

## Problem

`GET /api/settings` omits server-owned cluster CA material, while `PUT /api/settings` previously replaced the full settings document. A normal settings save could therefore remove the CA pair from persistent storage. The running process could continue using its cached CA, masking the failure until restart, when enrolled workers could be orphaned. Omitted ordinary settings also reverted to defaults.

## Implemented behavior

- The handler recursively fills omitted object fields from the stored settings document. Submitted scalars, arrays, and explicit nulls retain their normal replacement semantics.
- The service locks the authoritative settings row and retains its CA certificate, encrypted CA key, and join-token hash across generic settings saves. A forged or stale client value cannot replace these server-owned fields.
- Malformed stored settings fail closed before an update can remove cluster or token state.
- Join-token generation and revocation use a dedicated transactional update that changes only the hash in the locked JSON row, preserves unknown keys and CA material, and invalidates the settings cache. This also resolves follow-up issue https://github.com/gotempsh/temps/issues/1117.

## Verification

- `cargo test --lib -p temps-config`: 140 passed, 0 failed. Unit and real PostgreSQL integration tests cover omitted settings, stale saves, token generation/revocation, and malformed stored settings.
- `cargo check --lib -p temps-config`: passed. Normal commit hooks, including formatting and clippy, passed.
- `/start-temps` isolated slot 5: backend health, console, and web returned HTTP 200. Authenticated API generation returned a token whose SHA-256 hash matched the database row. A stale settings PUT preserved that hash; revocation removed it; another stale PUT did not restore it. No token or hash is included in this document.
- Security auditor reviewed the current diff and granted sign-off. CI and PR review remain merge gates.

## Operational boundary

The local smoke test did not restart an enrolled remote worker to verify mTLS after control-plane restart. The real database tests verify CA persistence; worker reconnect remains a useful system-level check before merge.
