//! Read generated numbered DASH references without assuming output directories.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub fn resources(output: &Path, index: usize) -> (PathBuf, Vec<PathBuf>) {
    let text = std::fs::read_to_string(output.join("manifest.mpd")).unwrap();
    let document = drmr_core::manifest_support::xml::fromstring(&text).unwrap();
    let templates = document.iter_local("SegmentTemplate");
    let template = templates[index];
    let initialization = output.join(template.get("initialization").unwrap());
    let pattern = Path::new(template.get("media").unwrap());
    let (prefix, suffix) = pattern
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .split_once("$Number$")
        .unwrap();
    let directory = output.join(pattern.parent().unwrap());
    let mut numbered = BTreeMap::new();
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap();
        if let Some(number) = name
            .strip_prefix(prefix)
            .and_then(|name| name.strip_suffix(suffix))
        {
            let number: u64 = number.parse().unwrap();
            assert!(numbered.insert(number, path).is_none());
        }
    }
    assert!(!numbered.is_empty());
    let start: u64 = template.get("startNumber").unwrap_or("1").parse().unwrap();
    for (ordinal, number) in numbered.keys().enumerate() {
        assert_eq!(*number, start + u64::try_from(ordinal).unwrap());
    }
    (initialization, numbered.into_values().collect())
}
