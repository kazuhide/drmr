//! A bounded observation of actual task ordinals, never inferred from totals.
use drmr_core::events::Record;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::progress) enum ItemState {
    Waiting,
    Active,
    Completed,
    Stopped,
}
impl ItemState {
    fn parse(value: &serde_json::Value) -> Option<Self> {
        match value.as_str()? {
            "waiting" => Some(Self::Waiting),
            "active" => Some(Self::Active),
            "completed" => Some(Self::Completed),
            "stopped" => Some(Self::Stopped),
            _ => None,
        }
    }

    fn token(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Active => "active",
            Self::Completed => "completed",
            Self::Stopped => "stopped",
        }
    }

    fn follows(self, previous: Self) -> bool {
        match previous {
            Self::Waiting => true,
            Self::Active => self != Self::Waiting,
            Self::Completed | Self::Stopped => self == previous,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Items([ItemState; 64]);
impl Items {
    pub(super) fn parse(record: &Record, total: u64, expected: [u64; 4]) -> Option<Self> {
        let values = record.get("unit_states")?.as_array()?;
        if values.len() != usize::try_from(total).ok()? || values.len() > 64 {
            return None;
        }
        let mut states = [ItemState::Waiting; 64];
        let mut counts = [0; 4];
        for (index, value) in values.iter().enumerate() {
            let state = ItemState::parse(value)?;
            states[index] = state;
            counts[match state {
                ItemState::Active => 0,
                ItemState::Waiting => 1,
                ItemState::Completed => 2,
                ItemState::Stopped => 3,
            }] += 1;
        }
        (counts == expected).then_some(Self(states))
    }

    pub(super) fn states(&self, total: u64) -> &[ItemState] {
        &self.0[..usize::try_from(total).expect("validated task count")]
    }

    pub(super) fn follows(self, previous: Self, total: u64) -> bool {
        self.states(total)
            .iter()
            .zip(previous.states(total))
            .all(|(next, old)| next.follows(*old))
    }

    pub(super) fn json(&self, total: u64) -> serde_json::Value {
        self.states(total)
            .iter()
            .map(|state| state.token())
            .collect()
    }
}
