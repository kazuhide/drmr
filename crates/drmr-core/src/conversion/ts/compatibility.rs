//! A private demux view after complete admission of original transport bytes.
use super::super::ConversionError;
use super::admission::{Admission, empty_payload_padding_shape};
use crate::concurrency::Stop;
use std::{
    collections::BTreeMap,
    io::{Read as _, Write as _},
    path::Path,
};

const BAD: ConversionError = ConversionError("ts-compatibility-view-invalid");

/// Remove only admitted zero-payload padding and adjust the affected PID counters.
/// Every other header field, PCR byte, section CRC and PES byte remains unchanged.
pub(in crate::conversion) fn demux_view(
    input: &Path,
    output: &Path,
    admission: &Admission,
    stop: &Stop,
) -> Result<(), ConversionError> {
    admission.finish()?;
    if !admission.has_empty_payload_padding() {
        return Err(BAD);
    }
    let input = super::super::files::regular_input(input)?;
    let length = input.metadata().map_err(|_| BAD)?.len();
    if length == 0 || length % 188 != 0 {
        return Err(BAD);
    }
    let mut source = std::io::BufReader::new(input);
    let mut result = std::io::BufWriter::new(super::super::files::create_new(output)?);
    let mut removed = BTreeMap::<u16, u8>::new();
    let mut packet = [0; 188];
    for _ in 0..length / 188 {
        if stop.is_set() {
            return Err(ConversionError("cancelled"));
        }
        source.read_exact(&mut packet).map_err(|_| BAD)?;
        let pid = (u16::from(packet[1] & 31) << 8) | u16::from(packet[2]);
        if packet[3] & 0x30 == 0x30 && packet[4] == 183 {
            if !empty_payload_padding_shape(&packet)
                || !admission.tracks.iter().any(|track| track.pid == pid)
            {
                return Err(BAD);
            }
            let count = removed.entry(pid).or_default();
            *count = (*count + 1) & 15;
            continue;
        }
        if let Some(&offset) = removed.get(&pid) {
            packet[3] = (packet[3] & 0xf0) | ((packet[3] & 15).wrapping_sub(offset) & 15);
        }
        result.write_all(&packet).map_err(|_| BAD)?;
    }
    result.flush().map_err(|_| BAD)
}
