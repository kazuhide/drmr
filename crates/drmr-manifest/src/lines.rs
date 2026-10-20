/// Physical line boundaries preserve source offsets. Syntax parsers separately
/// decide which line endings their format permits.
pub(crate) fn ending_length(bytes: &[u8], index: usize) -> usize {
    match bytes.get(index) {
        Some(b'\r') => 1 + usize::from(bytes.get(index + 1) == Some(&b'\n')),
        Some(b'\n') if index == 0 || bytes[index - 1] != b'\r' => 1,
        _ => 0,
    }
}

pub(crate) fn physical_lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut position = 0;
    std::iter::from_fn(move || {
        if position == bytes.len() {
            return None;
        }
        let start = position;
        let end = bytes[start..]
            .iter()
            .position(|byte| matches!(byte, b'\r' | b'\n'))
            .map_or(bytes.len(), |offset| start + offset);
        position = end + ending_length(bytes, end);
        Some(&bytes[start..end])
    })
}
