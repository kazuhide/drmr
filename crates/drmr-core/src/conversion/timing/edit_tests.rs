use super::*;

fn boxed(kind: [u8; 4], payload: Vec<u8>) -> Vec<u8> {
    let mut result = u32::try_from(payload.len() + 8)
        .unwrap()
        .to_be_bytes()
        .to_vec();
    result.extend(kind);
    result.extend(payload);
    result
}

fn init(version: u8, media_time: u64, handler: [u8; 4]) -> Vec<u8> {
    let mut tkhd = vec![0; 84];
    tkhd[12..16].copy_from_slice(&1_u32.to_be_bytes());
    let mut mdhd = vec![0; 24];
    mdhd[12..16].copy_from_slice(&90_000_u32.to_be_bytes());
    let mut hdlr = vec![0; 24];
    hdlr[8..12].copy_from_slice(&handler);
    let mut mdia = boxed(*b"mdhd", mdhd);
    mdia.extend(boxed(*b"hdlr", hdlr));
    let mut edit = vec![version, 0, 0, 0, 0, 0, 0, 1];
    if version == 0 {
        edit.extend(0_u32.to_be_bytes());
        edit.extend(u32::try_from(media_time).unwrap().to_be_bytes());
    } else {
        edit.extend(0_u64.to_be_bytes());
        edit.extend(media_time.to_be_bytes());
    }
    edit.extend([0, 1, 0, 0]);
    let mut track = boxed(*b"tkhd", tkhd);
    track.extend(boxed(*b"mdia", mdia));
    track.extend(boxed(*b"edts", boxed(*b"elst", edit)));
    let mut trex = vec![0; 24];
    trex[4..8].copy_from_slice(&1_u32.to_be_bytes());
    trex[8..12].copy_from_slice(&1_u32.to_be_bytes());
    trex[12..16].copy_from_slice(&3750_u32.to_be_bytes());
    let mut movie = boxed(*b"trak", track);
    movie.extend(boxed(*b"mvex", boxed(*b"trex", trex)));
    boxed(*b"moov", movie)
}

fn fragment(cts: u32) -> Vec<u8> {
    let mut tfhd = vec![0, 2, 0, 0];
    tfhd.extend(1_u32.to_be_bytes());
    let mut tfdt = vec![0; 4];
    tfdt.extend(900_000_u32.to_be_bytes());
    let mut run = vec![0, 0, 9, 0];
    run.extend(2_u32.to_be_bytes());
    for _ in 0..2 {
        run.extend(3750_u32.to_be_bytes());
        run.extend(cts.to_be_bytes());
    }
    let mut track = boxed(*b"tfhd", tfhd);
    track.extend(boxed(*b"tfdt", tfdt));
    track.extend(boxed(*b"trun", run));
    boxed(*b"moof", boxed(*b"traf", track))
}

#[test]
fn unbounded_rate_one_video_edits_map_fragment_times_to_the_movie_clock() {
    let owner = crate::native::private_tempdir().unwrap();
    let initialization = owner.path().join("init.mp4");
    let segment = owner.path().join("segment.m4s");
    std::fs::write(&segment, fragment(7500)).unwrap();
    for version in [0, 1] {
        for shift in [0_u64, 3000, 7500] {
            std::fs::write(&initialization, init(version, shift, *b"vide")).unwrap();
            let mut clock = FragmentClock::read(&initialization).unwrap();
            let first = 907_500 - shift;
            assert_eq!(
                clock.dash_clocks(&segment).unwrap(),
                ((first, 7500, 90_000), (first, first + 7500, 90_000)),
                "version={version}, edit={shift}"
            );
            assert_eq!(
                clock.presentation_window().unwrap(),
                (first, first + 7500, 90_000)
            );
        }
    }
}

#[test]
fn unbounded_audio_edits_keep_their_existing_movie_clock_mapping() {
    let owner = crate::native::private_tempdir().unwrap();
    let initialization = owner.path().join("init.mp4");
    let segment = owner.path().join("segment.m4s");
    std::fs::write(&segment, fragment(0)).unwrap();
    for version in [0, 1] {
        std::fs::write(&initialization, init(version, 7500, *b"soun")).unwrap();
        let mut clock = FragmentClock::read(&initialization).unwrap();
        assert_eq!(
            clock.dash_clocks(&segment).unwrap(),
            ((892_500, 7500, 90_000), (892_500, 900_000, 90_000))
        );
    }
}

#[test]
fn unsupported_fragment_edit_versions_retain_the_presentation_profile_error() {
    let owner = crate::native::private_tempdir().unwrap();
    let initialization = owner.path().join("init.mp4");
    for version in [2, 7, u8::MAX] {
        std::fs::write(&initialization, init(version, 7500, *b"vide")).unwrap();
        assert_eq!(
            FragmentClock::read(&initialization).err().unwrap().0,
            "presentation-window-not-supported"
        );
    }
}
