//! Temps CLI - Single entrypoint for all services
//!
//! This binary delegates to `temps_cli::run` (defined in `lib.rs`) so the
//! same dispatch can be reused by EE-bundled binaries that need to
//! register additional plugins. See ADR 0001 §"Extension points exposed
//! by OSS".

// jemalloc fragments far less than system malloc under the proxy's workload
// and its dirty-page decay returns freed memory to the OS (system allocators
// ratchet at the high-water mark). Heap profiling: set
//   _RJEM_MALLOC_CONF=prof:true,prof_active:true,lg_prof_sample:19,prof_prefix:/tmp/jeprof
// (inert / zero overhead when unset). Not available on MSVC.
#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn main() -> anyhow::Result<()> {
    temps_cli::run(Vec::new())
}
