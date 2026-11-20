// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Resource usage of a workload VM, read from the guest's `/proc`.
//!
//! The host cannot see inside the VM, and the VMM's own usage is not the
//! workload's, so the guest reports its own cumulative counters
//! ([`Request::ReadStats`](crate::Request::ReadStats)). Counters are
//! cumulative on purpose: the host turns two samples into a rate, so a
//! missed sample never loses usage.
//!
//! Cost: one pass over `/proc/<pid>/stat` for each process, plus three small
//! files. A workload VM runs a handful of processes, so a sample is a few
//! hundred microseconds and allocates only the process table it reads.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// What the guest reports for one sample. Every counter but the memory
/// figures is cumulative since boot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestStats {
    /// CPU time (user + system) of the workload's process tree.
    pub cpu_nanos: u64,
    /// Resident memory of the workload's process tree.
    pub memory_bytes: u64,
    /// The VM's total RAM, from `/proc/meminfo`.
    pub memory_total_bytes: u64,
    /// RAM the guest could still hand out without swapping.
    pub memory_available_bytes: u64,
    /// Processes in the workload's tree.
    pub pids: u32,
    /// Bytes read from, and written to, the VM's whole-disk block devices.
    pub disk_read_bytes: u64,
    pub disk_write_bytes: u64,
    /// Bytes received and sent on every interface but loopback.
    pub net_rx_bytes: u64,
    pub net_tx_bytes: u64,
}

/// Bytes in the sectors `/proc/diskstats` counts, whatever the device's
/// real sector size.
const DISKSTATS_SECTOR_BYTES: u64 = 512;

/// The units the kernel reports process times and memory in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcUnits {
    pub clock_ticks_per_sec: u64,
    pub page_bytes: u64,
}

impl ProcUnits {
    /// The running kernel's units; the usual 100 Hz and 4 KiB if it will not
    /// say.
    pub fn from_system() -> Self {
        // SAFETY: `sysconf` reads a constant and has no preconditions.
        let (ticks, page) = unsafe {
            (
                libc::sysconf(libc::_SC_CLK_TCK),
                libc::sysconf(libc::_SC_PAGESIZE),
            )
        };
        Self {
            clock_ticks_per_sec: u64::try_from(ticks).ok().filter(|t| *t > 0).unwrap_or(100),
            page_bytes: u64::try_from(page).ok().filter(|p| *p > 0).unwrap_or(4096),
        }
    }
}

/// One process, from `/proc/<pid>/stat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcessSample {
    pid: u32,
    ppid: u32,
    /// `utime + stime`, in clock ticks.
    cpu_ticks: u64,
    rss_pages: u64,
}

/// Parse one `/proc/<pid>/stat` line. The command name is parenthesised and
/// may itself contain spaces and parentheses, so the fields are counted from
/// the *last* `)`.
fn parse_process_stat(line: &str) -> Option<ProcessSample> {
    let open = line.find('(')?;
    let close = line.rfind(')')?;
    let pid = line.get(..open)?.trim().parse().ok()?;
    // After the command: state ppid pgrp session tty_nr tpgid flags minflt
    // cminflt majflt cmajflt utime stime cutime cstime priority nice
    // num_threads itrealvalue starttime vsize rss ...
    let fields: Vec<&str> = line.get(close + 1..)?.split_whitespace().collect();
    let field = |index: usize| fields.get(index)?.parse::<u64>().ok();
    // `rss` can be negative on a corrupt line; reject rather than wrap.
    Some(ProcessSample {
        pid,
        ppid: u32::try_from(field(1)?).ok()?,
        cpu_ticks: field(11)?.saturating_add(field(12)?),
        rss_pages: field(21)?,
    })
}

/// `MemTotal` and `MemAvailable`, in bytes.
fn parse_meminfo(text: &str) -> (u64, u64) {
    let kib = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
            .and_then(|rest| rest.split_whitespace().next()?.parse::<u64>().ok())
            .map_or(0, |value| value.saturating_mul(1024))
    };
    (kib("MemTotal"), kib("MemAvailable"))
}

/// Whether `/proc/diskstats` names a whole disk (`vda`, `sdb`, `xvdc`,
/// `nvme0n1`) rather than a partition, loop, ram or device-mapper device,
/// whose I/O is already counted on the disk beneath it.
fn is_whole_disk(name: &str) -> bool {
    let letters_only = |prefix: &str| {
        name.strip_prefix(prefix)
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_lowercase()))
    };
    if letters_only("vd") || letters_only("sd") || letters_only("xvd") {
        return true;
    }
    // nvme<controller>n<namespace>, without the trailing p<partition>.
    name.strip_prefix("nvme").is_some_and(|rest| {
        rest.split_once('n').is_some_and(|(controller, namespace)| {
            !controller.is_empty()
                && !namespace.is_empty()
                && controller.bytes().all(|b| b.is_ascii_digit())
                && namespace.bytes().all(|b| b.is_ascii_digit())
        })
    })
}

/// Bytes read and written across the whole disks in `/proc/diskstats`.
fn parse_diskstats(text: &str) -> (u64, u64) {
    let mut read = 0u64;
    let mut written = 0u64;
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let Some(name) = fields.get(2) else { continue };
        if !is_whole_disk(name) {
            continue;
        }
        let sectors = |index: usize| {
            fields
                .get(index)
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0)
        };
        read = read.saturating_add(sectors(5).saturating_mul(DISKSTATS_SECTOR_BYTES));
        written = written.saturating_add(sectors(9).saturating_mul(DISKSTATS_SECTOR_BYTES));
    }
    (read, written)
}

/// Bytes received and sent on every interface but loopback, from
/// `/proc/net/dev`.
fn parse_net_dev(text: &str) -> (u64, u64) {
    let mut rx = 0u64;
    let mut tx = 0u64;
    for line in text.lines().skip(2) {
        let Some((name, counters)) = line.split_once(':') else {
            continue;
        };
        if name.trim() == "lo" {
            continue;
        }
        let fields: Vec<&str> = counters.split_whitespace().collect();
        let counter = |index: usize| {
            fields
                .get(index)
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0)
        };
        rx = rx.saturating_add(counter(0));
        tx = tx.saturating_add(counter(8));
    }
    (rx, tx)
}

/// `root` and everything below it. With no `root`, every process counts: a
/// VM that has not spawned a workload yet still reports its own usage.
fn process_tree(
    processes: &[ProcessSample],
    root: Option<u32>,
) -> impl Iterator<Item = &ProcessSample> {
    let mut members: HashSet<u32> = HashSet::new();
    match root {
        None => members.extend(processes.iter().map(|p| p.pid)),
        Some(root) => {
            let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
            for process in processes {
                children.entry(process.ppid).or_default().push(process.pid);
            }
            let mut pending = vec![root];
            while let Some(pid) = pending.pop() {
                if !members.insert(pid) {
                    continue;
                }
                if let Some(kids) = children.get(&pid) {
                    pending.extend(kids.iter().copied());
                }
            }
        }
    }
    processes
        .iter()
        .filter(move |process| members.contains(&process.pid))
}

fn read_processes(proc_root: &Path) -> Vec<ProcessSample> {
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.bytes().all(|b| b.is_ascii_digit()))
        })
        // A process may exit between the listing and the read: skip it.
        .filter_map(|entry| std::fs::read_to_string(entry.path().join("stat")).ok())
        .filter_map(|line| parse_process_stat(&line))
        .collect()
}

/// Sample the guest, counting the process tree rooted at `root_pid` (or all
/// processes when `None`). `proc_root` is `/proc`, a parameter so a test can
/// point it at a fixture.
pub fn collect(proc_root: &Path, root_pid: Option<u32>, units: ProcUnits) -> GuestStats {
    let processes = read_processes(proc_root);
    let (mut cpu_ticks, mut rss_pages, mut pids) = (0u64, 0u64, 0u32);
    for process in process_tree(&processes, root_pid) {
        cpu_ticks = cpu_ticks.saturating_add(process.cpu_ticks);
        rss_pages = rss_pages.saturating_add(process.rss_pages);
        pids = pids.saturating_add(1);
    }
    let read = |name: &str| std::fs::read_to_string(proc_root.join(name)).unwrap_or_default();
    let (memory_total_bytes, memory_available_bytes) = parse_meminfo(&read("meminfo"));
    let (disk_read_bytes, disk_write_bytes) = parse_diskstats(&read("diskstats"));
    let (net_rx_bytes, net_tx_bytes) = parse_net_dev(&read("net/dev"));
    GuestStats {
        cpu_nanos: cpu_ticks
            .saturating_mul(1_000_000_000)
            .checked_div(units.clock_ticks_per_sec)
            .unwrap_or(0),
        memory_bytes: rss_pages.saturating_mul(units.page_bytes),
        memory_total_bytes,
        memory_available_bytes,
        pids,
        disk_read_bytes,
        disk_write_bytes,
        net_rx_bytes,
        net_tx_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNITS: ProcUnits = ProcUnits {
        clock_ticks_per_sec: 100,
        page_bytes: 4096,
    };

    /// `pid (comm) S ppid ...` with utime=`u`, stime=`s`, rss=`rss`.
    fn stat_line(pid: u32, comm: &str, ppid: u32, utime: u64, stime: u64, rss: u64) -> String {
        // state ppid pgrp session tty tpgid flags minflt cminflt majflt
        // cmajflt utime stime cutime cstime priority nice threads itreal
        // starttime vsize rss
        format!(
            "{pid} ({comm}) S {ppid} 1 1 0 -1 4194560 100 0 0 0 {utime} {stime} 0 0 20 0 1 0 500 1000000 {rss} 18446744073709551615"
        )
    }

    #[test]
    fn a_process_line_is_read_from_after_the_last_parenthesis() {
        let sample =
            parse_process_stat(&stat_line(42, "node (worker) x", 7, 150, 50, 300)).expect("parsed");
        assert_eq!(
            sample,
            ProcessSample {
                pid: 42,
                ppid: 7,
                cpu_ticks: 200,
                rss_pages: 300
            }
        );
    }

    #[test]
    fn a_truncated_process_line_is_rejected_not_guessed() {
        assert_eq!(parse_process_stat("42 (node) S 7 1"), None);
        assert_eq!(parse_process_stat("garbage"), None);
        assert_eq!(parse_process_stat(""), None);
    }

    #[test]
    fn meminfo_reports_bytes_and_a_missing_key_is_zero() {
        let text = "MemTotal:        2048 kB\nMemFree:  100 kB\nMemAvailable:    512 kB\n";
        assert_eq!(parse_meminfo(text), (2048 * 1024, 512 * 1024));
        assert_eq!(parse_meminfo("MemTotal: 10 kB\n"), (10 * 1024, 0));
        assert_eq!(parse_meminfo(""), (0, 0));
    }

    #[test]
    fn only_whole_disks_count_toward_disk_io() {
        for whole in ["vda", "vdb", "sda", "xvdc", "nvme0n1", "nvme12n3"] {
            assert!(is_whole_disk(whole), "{whole}");
        }
        for other in [
            "vda1",
            "sda2",
            "nvme0n1p1",
            "loop0",
            "ram0",
            "dm-0",
            "vd",
            "nvme",
            "nvme0",
            "",
        ] {
            assert!(!is_whole_disk(other), "{other}");
        }
    }

    #[test]
    fn diskstats_sums_sectors_of_whole_disks_only() {
        let text = "\
 252       0 vda 100 0 2000 50 40 0 800 30 0 80 80
 252       1 vda1 90 0 1900 40 30 0 700 20 0 60 60
   7       0 loop0 5 0 999 1 0 0 0 0 0 1 1
 252      16 vdb 10 0 100 5 20 0 400 10 0 15 15
";
        assert_eq!(
            parse_diskstats(text),
            ((2000 + 100) * 512, (800 + 400) * 512)
        );
        assert_eq!(parse_diskstats(""), (0, 0));
    }

    #[test]
    fn net_dev_skips_loopback_and_sums_the_rest() {
        let text = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 9999     10    0    0    0     0          0         0     9999     10    0    0    0     0       0          0
  eth0: 1000     10    0    0    0     0          0         0     2000     20    0    0    0     0       0          0
  eth1:  500      5    0    0    0     0          0         0      250      3    0    0    0     0       0          0
";
        assert_eq!(parse_net_dev(text), (1500, 2250));
        assert_eq!(parse_net_dev(""), (0, 0));
    }

    #[test]
    fn the_tree_follows_children_and_ignores_unrelated_processes() {
        let processes = [
            ProcessSample {
                pid: 1,
                ppid: 0,
                cpu_ticks: 5,
                rss_pages: 1,
            },
            ProcessSample {
                pid: 10,
                ppid: 1,
                cpu_ticks: 100,
                rss_pages: 10,
            },
            ProcessSample {
                pid: 11,
                ppid: 10,
                cpu_ticks: 50,
                rss_pages: 20,
            },
            ProcessSample {
                pid: 12,
                ppid: 11,
                cpu_ticks: 25,
                rss_pages: 30,
            },
            ProcessSample {
                pid: 20,
                ppid: 1,
                cpu_ticks: 999,
                rss_pages: 999,
            },
        ];
        let tree: Vec<u32> = process_tree(&processes, Some(10)).map(|p| p.pid).collect();
        assert_eq!(tree, vec![10, 11, 12]);
        assert_eq!(process_tree(&processes, None).count(), 5);
        // A root that is gone has an empty tree, not everything.
        assert_eq!(process_tree(&processes, Some(77)).count(), 0);
    }

    #[test]
    fn a_parent_cycle_cannot_loop_forever() {
        let processes = [
            ProcessSample {
                pid: 5,
                ppid: 6,
                cpu_ticks: 1,
                rss_pages: 1,
            },
            ProcessSample {
                pid: 6,
                ppid: 5,
                cpu_ticks: 1,
                rss_pages: 1,
            },
        ];
        assert_eq!(process_tree(&processes, Some(5)).count(), 2);
    }

    /// A fixture `/proc` laid out like the kernel's.
    fn fixture() -> tempfile_dir::Dir {
        let dir = tempfile_dir::Dir::new();
        dir.write("10/stat", &stat_line(10, "app", 1, 300, 100, 1000));
        dir.write("11/stat", &stat_line(11, "child", 10, 100, 0, 500));
        dir.write("20/stat", &stat_line(20, "other", 1, 9000, 9000, 9000));
        dir.write("meminfo", "MemTotal: 4096 kB\nMemAvailable: 1024 kB\n");
        dir.write("diskstats", "252 0 vda 1 0 8 0 2 0 16 0 0 0 0\n");
        dir.write(
            "net/dev",
            "h\nh\n  eth0: 100 1 0 0 0 0 0 0 200 2 0 0 0 0 0 0\n",
        );
        dir
    }

    #[test]
    fn a_sample_totals_the_workload_tree_and_the_vm_counters() {
        let dir = fixture();
        let stats = collect(dir.path(), Some(10), UNITS);
        assert_eq!(
            stats,
            GuestStats {
                // (300+100) + (100+0) ticks at 100 Hz = 5 s
                cpu_nanos: 5_000_000_000,
                memory_bytes: 1500 * 4096,
                memory_total_bytes: 4096 * 1024,
                memory_available_bytes: 1024 * 1024,
                pids: 2,
                disk_read_bytes: 8 * 512,
                disk_write_bytes: 16 * 512,
                net_rx_bytes: 100,
                net_tx_bytes: 200,
            }
        );
    }

    #[test]
    fn a_missing_proc_yields_zeroes_not_an_error() {
        let stats = collect(Path::new("/nonexistent/proc"), Some(1), UNITS);
        assert_eq!(stats, GuestStats::default());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_running_system_reports_this_process() {
        let stats = collect(
            Path::new("/proc"),
            Some(std::process::id()),
            ProcUnits::from_system(),
        );
        assert!(stats.pids >= 1, "{stats:?}");
        assert!(stats.memory_bytes > 0, "{stats:?}");
        assert!(stats.memory_total_bytes > 0, "{stats:?}");
    }

    /// A throwaway directory, removed on drop.
    mod tempfile_dir {
        use std::path::{Path, PathBuf};

        pub struct Dir(PathBuf);

        impl Dir {
            pub fn new() -> Self {
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let path = std::env::temp_dir().join(format!(
                    "vm-agent-stats-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                ));
                std::fs::create_dir_all(&path).expect("create fixture dir");
                Self(path)
            }

            pub fn path(&self) -> &Path {
                &self.0
            }

            pub fn write(&self, relative: &str, contents: &str) {
                let path = self.0.join(relative);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).expect("create fixture parent");
                }
                std::fs::write(path, contents).expect("write fixture file");
            }
        }

        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
