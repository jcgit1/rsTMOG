//! Filesystem helpers that take an explicit root path, so every collector
//! can be pointed at a fixture tree in tests (`/` in production).
//! Permission-denied reads are reported as `Io` errors and callers that
//! treat a file as optional tolerate them.

use std::io;
use std::path::{Path, PathBuf};

/// Read a whole file to a String (procfs/sysfs files are small; we do not
/// stream them).
pub fn read_to_string(path: &Path) -> io::Result<String> {
    std::fs::read_to_string(path)
}

/// Resolve `rel` under `root` and read it. `root` is `/` in production and
/// a fixture directory in tests.
pub fn read_under(root: &Path, rel: &str) -> io::Result<String> {
    read_to_string(&root.join(rel))
}

/// Read a u64 from a file, trimming whitespace. Sysfs counters are plain
/// decimal.
pub fn read_u64_under(root: &Path, rel: &str) -> io::Result<u64> {
    let text = read_under(root, rel)?;
    text.trim()
        .parse::<u64>()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// List immediate children of `root/rel` sorted by name (deterministic for
/// tests). Missing directory → empty list (many sysfs classes are absent
/// on a given box; optional by design).
pub fn list_dir_sorted(root: &Path, rel: &str) -> io::Result<Vec<PathBuf>> {
    let dir = root.join(rel);
    let mut out: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd.filter_map(|e| e.ok().map(|e| e.path())).collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fs")
    }

    #[test]
    fn read_under_and_u64() {
        let root = fixture_root();
        assert_eq!(read_under(&root, "stat_like.txt").unwrap(), "cpu 1 2 3 4\n");
        assert_eq!(read_u64_under(&root, "counter.txt").unwrap(), 4211);
    }

    #[test]
    fn missing_file_is_error_missing_dir_empty() {
        let root = fixture_root();
        assert!(read_under(&root, "nope.txt").is_err());
        let list = list_dir_sorted(&root, "absent_dir").unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn list_dir_sorted_is_deterministic() {
        let root = fixture_root();
        let children = list_dir_sorted(&root, "devices").unwrap();
        let names: Vec<String> = children
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        // lexicographic order — collectors that need numeric order sort
        // themselves (documented behavior, tests pin it).
        assert_eq!(names, vec!["cpu0", "cpu1", "cpu10"]);
    }
}
