//! Retained inventories and identity maps, separate from streaming leaf working sets.
use super::{
    ConversionError, FileIdentity, MAX_FILES, MAX_PATH_BYTES, PathBuf, SourceFile, SourceInventory,
};

const BAD: ConversionError = ConversionError("resource-budget");

fn map_entry_bytes() -> usize {
    // B-tree occupancy, child pointers and allocation headers are conservative
    // estimates. An identity map also owns a second relative-path allocation.
    3 * (std::mem::size_of::<PathBuf>()
        + std::mem::size_of::<FileIdentity>()
        + 8 * std::mem::size_of::<usize>())
}

pub(in crate::conversion) fn retained_inventory_bytes(
    inventory: &SourceInventory,
) -> Result<u64, ConversionError> {
    let mut bytes = inventory
        .files
        .capacity()
        .checked_mul(std::mem::size_of::<SourceFile>())
        .ok_or(BAD)?;
    for size in [
        std::mem::size_of::<super::OriginalGuard>(),
        inventory.root.capacity(),
        inventory.entrypoint.capacity(),
        inventory.graph_digest.capacity(),
    ] {
        bytes = bytes.checked_add(size).ok_or(BAD)?;
    }
    for file in &inventory.files {
        bytes = bytes
            .checked_add(map_entry_bytes())
            .and_then(|n| n.checked_add(file.path.capacity().checked_mul(2)?))
            .and_then(|n| n.checked_add(file.sha256.capacity()))
            .ok_or(BAD)?;
    }
    u64::try_from(bytes).map_err(|_| BAD)
}

pub(in crate::conversion) fn retained_inventory_bound(
    single_file: bool,
) -> Result<u64, ConversionError> {
    let files = if single_file { 1 } else { MAX_FILES };
    let paths = if single_file { 4096 } else { MAX_PATH_BYTES };
    // Growing inventory Vec, 64-byte digest strings, identity-map allocations,
    // doubled path capacity in both owners, and bounded root/entrypoint state.
    let entries = 2 * std::mem::size_of::<SourceFile>() + 64 + map_entry_bytes();
    let bytes = files
        .checked_mul(entries)
        .and_then(|n| n.checked_add(paths.checked_mul(4)?))
        .and_then(|n| n.checked_add(256 * 1024))
        .ok_or(BAD)?;
    u64::try_from(bytes).map_err(|_| BAD)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn result_inventory_bounds_cover_retained_paths_digests_and_identity_maps() {
        let files = (0..200)
            .map(|n| SourceFile {
                path: PathBuf::from(format!("track-01/segment-{n:06}.m4s")),
                bytes: 1024,
                sha256: "a".repeat(64),
            })
            .collect();
        let mut inventory = SourceInventory {
            schema_version: 1,
            root: PathBuf::from("/source"),
            entrypoint: PathBuf::from("manifest.mpd"),
            files,
            graph_digest: "b".repeat(64),
        };
        let small = retained_inventory_bytes(&inventory).unwrap();
        inventory.files[0].path.reserve(4096);
        assert!(retained_inventory_bytes(&inventory).unwrap() > small);
        assert!(
            retained_inventory_bound(false).unwrap()
                > retained_inventory_bytes(&inventory).unwrap()
        );
        assert!(retained_inventory_bound(false).unwrap() < 128 * 1024 * 1024);
        assert!(retained_inventory_bound(true).unwrap() < 1024 * 1024);
    }
}
