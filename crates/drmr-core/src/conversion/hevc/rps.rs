//! Short-term reference picture sets, including inter-RPS prediction.
use super::{BAD, ConversionError, bits::Bits};
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Rps {
    pub pictures: Vec<(i32, bool)>,
}
impl Rps {
    pub fn used(&self) -> u32 {
        self.pictures.iter().filter(|(_, used)| *used).count() as u32
    }
}
pub(super) fn parse(
    b: &mut Bits<'_>,
    previous: &[Rps],
    in_slice: bool,
    max_buffer: u32,
) -> Result<Rps, ConversionError> {
    let index = previous.len();
    let predicted = index != 0 && b.flag()?;
    let mut pictures = Vec::new();
    if predicted {
        let delta_index = if in_slice {
            b.ue(u32::try_from(index - 1).map_err(|_| BAD)?)? as usize + 1
        } else {
            1
        };
        let reference = previous
            .get(index.checked_sub(delta_index).ok_or(BAD)?)
            .ok_or(BAD)?;
        let negative = b.flag()?;
        let delta =
            i32::try_from(b.ue(32767)? + 1).map_err(|_| BAD)? * if negative { -1 } else { 1 };
        for old in reference
            .pictures
            .iter()
            .map(|(poc, _)| *poc)
            .chain(std::iter::once(0))
        {
            let used = b.flag()?;
            let keep = used || b.flag()?;
            let poc = old.checked_add(delta).ok_or(BAD)?;
            if keep && poc != 0 {
                pictures.push((poc, used));
            }
        }
        // The inference order is negative deltas nearest first, then positive deltas.
        pictures.sort_unstable_by_key(|(poc, _)| {
            if *poc < 0 {
                (0, -i64::from(*poc))
            } else {
                (1, i64::from(*poc))
            }
        });
        if pictures.windows(2).any(|p| p[0].0 == p[1].0) {
            return Err(BAD);
        }
    } else {
        let negative = b.ue(max_buffer)?;
        let positive = b.ue(max_buffer.checked_sub(negative).ok_or(BAD)?)?;
        let mut poc = 0_i32;
        for _ in 0..negative {
            poc = poc
                .checked_sub(i32::try_from(b.ue(32767)? + 1).map_err(|_| BAD)?)
                .ok_or(BAD)?;
            pictures.push((poc, b.flag()?));
        }
        let mut poc = 0_i32;
        for _ in 0..positive {
            poc = poc
                .checked_add(i32::try_from(b.ue(32767)? + 1).map_err(|_| BAD)?)
                .ok_or(BAD)?;
            pictures.push((poc, b.flag()?));
        }
    }
    if pictures.len() > max_buffer as usize {
        return Err(BAD);
    }
    Ok(Rps { pictures })
}
