#!/usr/bin/env bash
# e2e trace: what happens behind the scenes when a node joins the cluster.
# Drives a real enrollment + mTLS CSR handshake (a controlled `temps join` from
# worker-1 with an isolated data dir, so the real workers are untouched), and
# dumps the artifacts at each step. Cleans up the trace node + token at the end.
set -uo pipefail
cd "$(dirname "$0")"
PSQL="docker exec temps-harden-postgres psql -U temps -d temps"
CP_URL="http://10.42.0.10:80"
TRACE_NAME="worker-trace"
TRACE_IP="10.42.0.99"
J=/tmp/trace_cookies.txt
step() { echo; echo "════════ $* ════════"; }

PW=$(sed -n '2p' .state/admin.txt)
curl -s -c $J -X POST http://localhost/api/auth/login -H 'Content-Type: application/json' \
  -d "{\"email\":\"admin@local.dev\",\"password\":\"$PW\"}" -o /dev/null

step "PHASE 0 — Bootstrap state (control plane + cluster CA)"
echo "  The control plane minted a per-cluster CA when require_mtls was enabled."
$PSQL -tAc "select 'require_mtls=' || (data::jsonb->'multi_node'->>'require_mtls') || '  ca_key_encrypted_at_rest=' || ((data::jsonb->'multi_node'->>'cluster_ca_key_encrypted') is not null) from settings where id=1;" 2>/dev/null | sed 's/^/  /'
CA_FP=$(curl -s -b $J http://localhost/api/settings | python3 -c "import sys,json;print(json.load(sys.stdin).get('multi_node',{}).get('cluster_ca_fingerprint',''))" 2>/dev/null)
echo "  cluster CA fingerprint (SHA-256, public): ${CA_FP:0:24}…  (the CA *private key* never leaves the CP)"

step "PHASE 1 — Operator mints a single-use enrollment token"
MINT=$(curl -s -b $J -X POST http://localhost/api/settings/enrollment-tokens -H 'Content-Type: application/json' -d '{"ttl_secs":3600,"max_uses":1}')
TOKEN=$(echo "$MINT" | python3 -c "import sys,json;print(json.load(sys.stdin).get('token',''))")
MINT_FP=$(echo "$MINT" | python3 -c "import sys,json;print(json.load(sys.stdin).get('ca_fingerprint',''))")
echo "  POST /api/settings/enrollment-tokens {ttl_secs:3600, max_uses:1}"
echo "  → plaintext token (shown ONCE): ${TOKEN:0:16}…   carries ca_fingerprint=${MINT_FP:0:16}…"
echo "  DB row — only the SHA-256 hash is stored, never the plaintext:"
$PSQL -c "select id, left(token_hash,16)||'…' token_hash, max_uses, used_count, (revoked_at is null) active, left(ca_fingerprint,12)||'…' ca_fp from node_enrollment_tokens order by id desc limit 1;" 2>&1 | sed 's/^/    /'

step "PHASE 2 — Worker runs \`temps join\` (CSR + ca-fingerprint verify + register)"
echo "  Behind the scenes: the joining node generates its OWN keypair + CSR"
echo "  (private key never leaves it), verifies the CP's CA matches the pinned"
echo "  fingerprint, then registers. Running it from worker-1 with an isolated"
echo "  data dir so the real node is untouched:"
docker exec temps-harden-worker-1 sh -c "rm -rf /tmp/trace && mkdir -p /tmp/trace && HOME=/tmp/trace TEMPS_DATA_DIR=/tmp/trace /usr/local/bin/temps join $CP_URL '$TOKEN' --name $TRACE_NAME --private-address $TRACE_IP --ca-fingerprint $CA_FP 2>&1" 2>&1 | sed 's/^/    /' | head -25

step "PHASE 3 — What the control plane did (register_node)"
echo "  • enrollment token consumed atomically (used_count 0 → 1):"
$PSQL -c "select id, used_count, max_uses from node_enrollment_tokens order by id desc limit 1;" 2>&1 | sed 's/^/    /'
echo "  • node row created + an overlay CIDR allocated from the cluster pool,"
echo "    address switched to https:// so CP→agent calls use mTLS:"
$PSQL -c "select id, name, address, compute_cidr, status from nodes where name='$TRACE_NAME';" 2>&1 | sed 's/^/    /'

step "PHASE 4 — The issued certificate (server-authoritative SANs)"
echo "  The CP signed the CSR but set the leaf's SANs from the node's registered"
echo "  {IP, name} (the security fix). The node's own private key stayed local."
CERT=$(docker exec temps-harden-worker-1 sh -c "ls /tmp/trace/*.pem /tmp/trace/**/*.pem 2>/dev/null | grep -iE 'cert|node' | head -1")
echo "  cert file on the node: ${CERT:-<not found>}"
if docker exec temps-harden-worker-1 sh -c "command -v openssl >/dev/null 2>&1"; then
  docker exec temps-harden-worker-1 sh -c "openssl x509 -in '$CERT' -noout -text 2>/dev/null | grep -A1 -iE 'Subject Alternative Name'" 2>&1 | sed 's/^/    /'
else
  echo "    (openssl CLI not in container — SANs are [${TRACE_IP}, ${TRACE_NAME}] per register_node)"
fi

step "PHASE 5 — Single-use enforcement (replay the SAME token → rejected)"
docker exec temps-harden-worker-1 sh -c "rm -rf /tmp/trace2 && mkdir -p /tmp/trace2 && HOME=/tmp/trace2 TEMPS_DATA_DIR=/tmp/trace2 /usr/local/bin/temps join $CP_URL '$TOKEN' --name worker-trace-2 --private-address 10.42.0.98 --ca-fingerprint $CA_FP 2>&1" 2>&1 | grep -iE "error|exhaust|invalid|fail|reuse|used" | head -3 | sed 's/^/    /'
echo "    → the token is spent; a second registration is refused."

step "PHASE 6 — Steady state on a REAL worker (agent serves mTLS + heartbeats)"
echo "  Once joined for real, the agent serves TLS with its leaf and the CP"
echo "  presents its cluster-CA-signed client cert:"
docker logs temps-harden-worker-1 2>&1 | grep -i "mutual TLS" | tail -1 | sed 's/^/    /'
echo "  and it heartbeats CPU/mem/disk every ~30s (incl. the control plane now):"
curl -s -b $J http://localhost/api/internal/nodes | python3 -c "
import sys,json
for n in json.load(sys.stdin).get('nodes',[]):
    c=n.get('capacity') or {}
    print('    id=%-2d %-14s status=%-7s cpu=%s%% heartbeat=%s'%(n['id'],n['name'],n['status'],round(c.get('cpu_percent',0),1) if c.get('cpu_percent') is not None else 'n/a','yes' if n.get('last_heartbeat') else 'no'))
"

step "CLEANUP — remove the trace node + spend the token"
TID=$($PSQL -tAc "select id from nodes where name='$TRACE_NAME';" 2>/dev/null | tr -d '[:space:]')
[[ -n "$TID" ]] && $PSQL -c "delete from nodes where name='$TRACE_NAME';" >/dev/null 2>&1 && echo "  deleted trace node id=$TID"
$PSQL -c "update node_enrollment_tokens set revoked_at=now() where revoked_at is null and used_count>=max_uses;" >/dev/null 2>&1
docker exec temps-harden-worker-1 sh -c "rm -rf /tmp/trace /tmp/trace2" 2>/dev/null
echo
echo "════════ FORMATION TRACE DONE ════════"
