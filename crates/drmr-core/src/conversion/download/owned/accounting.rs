//! One metadata ceiling covers the graph, ownership, and overlapping verification paths.
use super::{ConversionError, Path, PathBuf};
use crate::concurrency::MAX_PENDING_TASKS;

const LIMIT: ConversionError = ConversionError("download-graph-limit");

pub(super) struct Accounting {
    graph: usize,
    owned: usize,
    base: usize,
    native_path: usize,
    wide_path: usize,
    depth: usize,
    pub maximum: usize,
}

impl Accounting {
    pub fn new(root_capacity: usize) -> Result<Self, ConversionError> {
        let base = root_capacity
            .checked_mul(4)
            .and_then(|n| n.checked_add(128 * 1024))
            .ok_or(LIMIT)?;
        let value = Self {
            graph: 0,
            owned: 0,
            base,
            native_path: 0,
            wide_path: 0,
            depth: 0,
            maximum: super::super::GRAPH_BYTES,
        };
        value.check(0, 0, 0, 0, 0)?;
        Ok(value)
    }

    fn check(
        &self,
        graph: usize,
        owned: usize,
        native: usize,
        wide: usize,
        depth: usize,
    ) -> Result<(), ConversionError> {
        // Queued closures and active sinks each own a full absolute path. Leave
        // room for recursive inventory frames and Windows' UTF-16 conversion.
        let paths = native
            .checked_mul(3)
            .and_then(|n| n.checked_add(wide))
            .and_then(|n| n.checked_mul(MAX_PENDING_TASKS))
            .ok_or(LIMIT)?;
        self.base
            .checked_add(graph)
            .and_then(|n| n.checked_add(owned))
            .and_then(|n| n.checked_add(paths))
            .and_then(|n| {
                depth
                    .checked_add(1)
                    .and_then(|d| d.checked_mul(64 * 1024))
                    .and_then(|d| n.checked_add(d))
            })
            .filter(|n| *n <= self.maximum)
            .map(|_| ())
            .ok_or(LIMIT)
    }

    pub fn graph(&mut self, bytes: usize) -> Result<(), ConversionError> {
        let graph = self.graph.checked_add(bytes).ok_or(LIMIT)?;
        self.check(
            graph,
            self.owned,
            self.native_path,
            self.wide_path,
            self.depth,
        )?;
        self.graph = graph;
        Ok(())
    }

    pub fn expansion(&self, bytes: usize) -> Result<(), ConversionError> {
        self.check(
            self.graph.checked_add(bytes).ok_or(LIMIT)?,
            self.owned,
            self.native_path,
            self.wide_path,
            self.depth,
        )
    }

    pub fn prepay(
        &mut self,
        root: &Path,
        paths: &[(PathBuf, bool)],
    ) -> Result<(), ConversionError> {
        let mut owned = self.owned;
        let mut native = self.native_path;
        let mut wide = self.wide_path;
        let mut depth = self.depth;
        for (path, directory) in paths {
            let storage = path.capacity().checked_mul(2).ok_or(LIMIT)?;
            // B-tree occupancy and cleanup's sorted borrowed-entry Vec coexist.
            let mut bytes = storage.checked_add(1024).ok_or(LIMIT)?;
            if !directory {
                // The digest map owns another key. Inventory has a growing Vec
                // of PathBufs while both maps remain alive, including spare capacity.
                bytes = bytes
                    .checked_add(storage.checked_mul(3).ok_or(LIMIT)?)
                    .and_then(|n| n.checked_add(1024))
                    .ok_or(LIMIT)?;
            }
            owned = owned.checked_add(bytes).ok_or(LIMIT)?;
            depth = depth.max(path.components().count());
            let absolute = root.join(path);
            // Private staging adds bounded generated directory/file suffixes.
            native = native.max(
                absolute
                    .capacity()
                    .checked_add(256)
                    .and_then(|n| n.checked_mul(2))
                    .ok_or(LIMIT)?,
            );
            let wide_bytes = {
                #[cfg(windows)]
                {
                    use std::os::windows::ffi::OsStrExt as _;
                    absolute
                        .as_os_str()
                        .encode_wide()
                        .count()
                        .checked_add(257)
                        .and_then(|n| n.checked_mul(2))
                        .ok_or(LIMIT)?
                }
                #[cfg(not(windows))]
                {
                    0
                }
            };
            wide = wide.max(wide_bytes);
        }
        self.check(self.graph, owned, native, wide, depth)?;
        self.owned = owned;
        self.native_path = native;
        self.wide_path = wide;
        self.depth = depth;
        Ok(())
    }

    pub fn finish(&mut self) -> Result<u64, ConversionError> {
        self.graph = 0;
        // Only inventory recursion remains after all queued fetches have joined.
        self.base
            .checked_add(self.owned)
            .and_then(|n| {
                self.native_path
                    .checked_mul(2)
                    .and_then(|p| p.checked_add(self.wide_path))
                    .and_then(|p| p.checked_add(64 * 1024))
                    .and_then(|p| self.depth.checked_add(1).and_then(|d| p.checked_mul(d)))
                    .and_then(|p| n.checked_add(p))
            })
            .filter(|n| *n <= self.maximum)
            .and_then(|n| u64::try_from(n).ok())
            .ok_or(LIMIT)
    }
}
