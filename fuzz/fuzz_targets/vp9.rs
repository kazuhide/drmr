#![no_main]
use drmr_core::conversion::fuzzing;
use libfuzzer_sys::fuzz_target;

fuzz_target!(
    init: {
        let (accepted, refused) = fuzzing::replay_vp9_seeds();
        eprintln!("DRMR_FUZZ_CORPUS target=vp9 accepted={accepted} refused={refused}");
    },
    |input: &[u8]| {
        let _ = fuzzing::vp9(input);
    }
);
