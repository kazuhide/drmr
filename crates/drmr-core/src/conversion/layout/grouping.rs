//! Transport playlists can retain one physical rendition containing two logical streams.
use super::{ConversionError, INVALID, paths};
use crate::{conversion::ProcessingMode, inspection::OutputFormat};
use std::{collections::BTreeMap, path::Path};

pub(super) struct Groups {
    pub ordinals: Vec<usize>,
    pub first: Vec<usize>,
}

impl Groups {
    pub fn resolve<'a>(
        format: OutputFormat,
        processing: ProcessingMode,
        sources: impl IntoIterator<Item = &'a Path>,
    ) -> Result<Self, ConversionError> {
        if processing == ProcessingMode::Auto {
            return Err(INVALID);
        }
        let mut seen = BTreeMap::new();
        let mut ordinals = Vec::new();
        let mut first = Vec::new();
        for (ordinal, source) in sources.into_iter().enumerate() {
            let alias = paths::alias(source)?;
            let group = if format == OutputFormat::HlsTs && processing == ProcessingMode::Segments {
                if let Some((original, group)) = seen.get(&alias) {
                    if *original != source {
                        return Err(INVALID);
                    }
                    *group
                } else {
                    let group = first.len();
                    seen.insert(alias, (source, group));
                    first.push(ordinal);
                    group
                }
            } else {
                first.push(ordinal);
                ordinal
            };
            ordinals.push(group);
        }
        Ok(Self { ordinals, first })
    }

    pub fn from_saved(ordinals: impl IntoIterator<Item = usize>) -> Result<Self, ConversionError> {
        let mut groups = Self {
            ordinals: Vec::new(),
            first: Vec::new(),
        };
        for (ordinal, group) in ordinals.into_iter().enumerate() {
            if group > groups.first.len() {
                return Err(INVALID);
            }
            if group == groups.first.len() {
                groups.first.push(ordinal);
            }
            groups.ordinals.push(group);
        }
        Ok(groups)
    }
}
