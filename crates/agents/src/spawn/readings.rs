//! Free RAM and free disk, read live at every spawn (P03). A reading that
//! fails is `None`, and a floor with no reading fails closed in `run.start`.

use std::path::Path;
use std::process::{Command, Stdio};

/// Free disk in MB on the file system that holds `path`, from `df -Pk`.
pub fn disk_free_mb(path: &Path) -> Option<i64> {
    let out = Command::new("df")
        .arg("-Pk")
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_df(&String::from_utf8_lossy(&out.stdout))
}

/// The available column of `df -P` output, in MB.
fn parse_df(text: &str) -> Option<i64> {
    let line = text.lines().nth(1)?;
    let kb: i64 = line.split_whitespace().nth(3)?.parse().ok()?;
    Some(kb / 1024)
}

/// Memory available to new processes, in MB.
pub fn ram_free_mb() -> Option<i64> {
    if let Ok(text) = std::fs::read_to_string("/proc/meminfo") {
        return parse_meminfo(&text);
    }
    let out = Command::new("vm_stat").stdin(Stdio::null()).output().ok()?;
    out.status
        .success()
        .then(|| parse_vm_stat(&String::from_utf8_lossy(&out.stdout)))
        .flatten()
}

/// Linux: `MemAvailable`, the kernel's own estimate.
fn parse_meminfo(text: &str) -> Option<i64> {
    let line = text.lines().find(|l| l.starts_with("MemAvailable:"))?;
    let kb: i64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024)
}

/// macOS: free, inactive and speculative pages times the page size.
fn parse_vm_stat(text: &str) -> Option<i64> {
    let page: i64 = text
        .lines()
        .next()?
        .split("page size of ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let pages = |name: &str| -> Option<i64> {
        let line = text.lines().find(|l| l.starts_with(name))?;
        line.rsplit(':')
            .next()?
            .trim()
            .trim_end_matches('.')
            .parse()
            .ok()
    };
    let free = pages("Pages free")? + pages("Pages inactive")? + pages("Pages speculative")?;
    Some(free * page / (1024 * 1024))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings_parse_each_platform_form() {
        let df = "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/sda1 100000000 60000000 41943040 59% /\n";
        assert_eq!(parse_df(df), Some(40960));
        let meminfo =
            "MemTotal:       32000000 kB\nMemFree:  100 kB\nMemAvailable:    3145728 kB\n";
        assert_eq!(parse_meminfo(meminfo), Some(3072));
        let vm = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free:                               65536.\nPages active:                            999999.\nPages inactive:                           65536.\nPages speculative:                        65536.\n";
        assert_eq!(parse_vm_stat(vm), Some(3072));
        assert_eq!(parse_df("garbage"), None);
    }
}
