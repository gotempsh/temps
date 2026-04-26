# DinD Integration Tests

End-to-end tests that exercise `temps-network` against a real Linux kernel
and a real Docker daemon, on any host that can run Docker.

## How it works

Two privileged `docker:dind` containers act as nodes A and B. They are
connected by a custom Docker bridge network (`temps-it-underlay`) which
plays the role of the cloud's underlay network. Each DinD has a stable
underlay IP (e.g. `10.123.0.2` and `10.123.0.3`).

A third container — the *runner* — holds the temps source tree and a Rust
toolchain. The runner ssh-execs into each DinD with `docker exec` to
configure the bridge / VXLAN / Docker network on each side, then runs
`docker exec` again to start nginx in node-A's Docker daemon and curl from
node-B's. Pings across the overlay must succeed.

```
                       host docker daemon
   ┌──────────────────────────────────────────────────────────────┐
   │                                                              │
   │   ┌─────────────── temps-it-underlay (10.123.0.0/24) ──┐     │
   │   │                                                    │     │
   │   │  ┌─── node-a (10.123.0.2) ─┐  ┌─── node-b (10.123.0.3) ─┐
   │   │  │ docker:dind             │  │ docker:dind             │
   │   │  │   inner-container nginx │  │   inner-container alpine│
   │   │  └─────────────────────────┘  └─────────────────────────┘
   │   └────────────────────────────────────────────────────┘     │
   │                                                              │
   │   ┌─── runner ──────────────────────────────────────────┐    │
   │   │ rust toolchain + temps source                       │    │
   │   │ orchestrates the test by calling docker exec        │    │
   │   └─────────────────────────────────────────────────────┘    │
   └──────────────────────────────────────────────────────────────┘
```

## Run

```bash
# From the repo root, on a host with Docker:
./crates/temps-network/tests/dind/run.sh
```

The script:

1. Builds a single Linux test binary by running `cargo build --tests` inside
   a builder container that shares the workspace cache.
2. Starts two privileged DinD containers on a dedicated bridge.
3. Copies the test binary into each DinD.
4. Inside each DinD, runs the binary with a different scenario:
   - Node A: bootstrap, then assert
   - Node B: bootstrap, then ping node A
5. Captures stdout from both, prints a verdict.

The runner exits non-zero if any assertion fails, with logs from both DinDs.

## Skip when Docker is unavailable

The Rust integration test (`it_multi_host.rs`) checks `docker version` at
startup and prints `[skip] docker daemon not available` when missing,
following the project's "tests skip gracefully" convention. CI sets
`TEMPS_RUN_DIND_TESTS=1` to require the test to actually run.

## What is asserted

| # | Scenario | Asserts |
|---|---|---|
| 1 | Bootstrap on a single node | `br-temps0` exists, has correct addr, MTU; `vxlan-temps0` has correct VNI; nftables `temps_network` table exists |
| 2 | Bootstrap is idempotent | Calling `bootstrap` twice with the same args is a no-op |
| 3 | Two-node cross-node ping | Container on node A pings container on node B by IP |
| 4 | reconcile_peers adds new peer | Adding a third peer entry installs FDB + route without disrupting existing flows |
| 5 | reconcile_peers removes peer | Removing a peer cleans up FDB + route |
| 6 | teardown is idempotent | Calling teardown twice doesn't error |
| 7 | Docker network CIDR collision | Pre-creating a Docker network on the same CIDR causes `bootstrap` to return `DockerCidrCollision` rather than corrupting state |
| 8 | Subnet validation | Calling `bootstrap` with a bridge address outside the compute CIDR returns `InvalidConfig` |
