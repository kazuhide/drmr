//! Only enum classifications and bounded numeric dimensions can become labels.
use super::ItemState;
use drmr_core::{events::Record, inspection::TrackKind};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::progress) struct Description {
    pub kind: TrackKind,
    pub resolution: Option<[u32; 2]>,
}
impl Description {
    fn parse(value: &serde_json::Value) -> Option<Self> {
        let object = value.as_object()?;
        let kind = match object.get("kind")?.as_str()? {
            "video" => TrackKind::Video,
            "audio" => TrackKind::Audio,
            "text" => TrackKind::Text,
            _ => return None,
        };
        let resolution = if let Some(value) = object.get("resolution") {
            let dimensions = value.as_array()?;
            if kind != TrackKind::Video || dimensions.len() != 2 {
                return None;
            }
            let width = u32::try_from(dimensions[0].as_u64()?).ok()?;
            let height = u32::try_from(dimensions[1].as_u64()?).ok()?;
            if width == 0 || height == 0 {
                return None;
            }
            Some([width, height])
        } else {
            None
        };
        Some(Self { kind, resolution })
    }

    fn json(self) -> serde_json::Value {
        let mut value = serde_json::json!({"kind":self.kind.as_str()});
        if let Some(resolution) = self.resolution {
            value["resolution"] = serde_json::json!(resolution);
        }
        value
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Descriptions([Option<Description>; 64]);
impl Descriptions {
    pub(super) fn parse(record: &Record, total: u64, actual_states: bool) -> Option<Option<Self>> {
        let Some(value) = record.get("unit_descriptions") else {
            return Some(None);
        };
        let values = value.as_array()?;
        if !actual_states || values.len() != usize::try_from(total).ok()? || values.len() > 64 {
            return None;
        }
        let mut descriptions = [None; 64];
        for (ordinal, value) in values.iter().enumerate() {
            if !value.is_null() {
                descriptions[ordinal] = Some(Description::parse(value)?);
            }
        }
        Some(Some(Self(descriptions)))
    }

    pub(super) fn get(&self, ordinal: usize, total: u64) -> Option<Description> {
        (ordinal < usize::try_from(total).expect("validated task count"))
            .then(|| self.0[ordinal])
            .flatten()
    }

    pub(super) fn follows(
        &self,
        previous: Option<&Self>,
        states: Option<&[ItemState]>,
        total: u64,
    ) -> bool {
        (0..usize::try_from(total).expect("validated task count")).all(|ordinal| {
            let old = previous.and_then(|previous| previous.0[ordinal]);
            match (old, self.0[ordinal]) {
                (Some(old), next) => Some(old) == next,
                (None, Some(_)) => states.is_none_or(|states| {
                    !matches!(states[ordinal], ItemState::Completed | ItemState::Stopped)
                }),
                (None, None) => true,
            }
        })
    }

    pub(super) fn json(&self, total: u64) -> serde_json::Value {
        self.0[..usize::try_from(total).expect("validated task count")]
            .iter()
            .map(|description| description.map_or(serde_json::Value::Null, Description::json))
            .collect()
    }
}
