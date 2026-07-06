//! Temps CLI - Single entrypoint for all services
//!
//! This binary delegates to `temps_cli::run` (defined in `lib.rs`) so the
//! same dispatch can be reused by EE-bundled binaries that need to
//! register additional plugins. See ADR 0001 §"Extension points exposed
//! by OSS".

// jemalloc fragments far less than glibc malloc under the proxy's workload
// (many small, short-lived per-request allocations across worker threads at
// sustained req/s), which is a meaningful share of the process's resident
// memory. Not available on MSVC.
//
// Heap profiling: macOS `leaks`/MallocStackLogging cannot see jemalloc
// allocations. Instead the allocator is built with the `profiling` and
// `stats` features (see workspace Cargo.toml) and instrumented at process
// start via the env var (symbols carry tikv's `_rjem_` prefix):
//   _RJEM_MALLOC_CONF=prof:true,prof_active:true,lg_prof_sample:19,prof_prefix:/tmp/jeprof
// Dumps are pprof-compatible; both knobs are inert (zero overhead) when the
// env var is unset.
#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn main() -> anyhow::Result<()> {
    temps_cli::run(Vec::new())
}
