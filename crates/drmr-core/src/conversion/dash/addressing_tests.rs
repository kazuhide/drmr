use super::*;

fn presentation(addressing: &str) -> String {
    format!(
        "<MPD type='static' mediaPresentationDuration='PT12S'><Period><AdaptationSet><Representation id='v' bandwidth='1'>{addressing}</Representation></AdaptationSet></Period></MPD>"
    )
}

#[test]
fn segment_list_enumerates_explicit_resources_with_exact_timing() {
    let xml = presentation(
        "<SegmentList timescale='1000'><Initialization sourceURL='init.mp4'/><SegmentTimeline><S t='0' d='6000' r='1'/></SegmentTimeline><SegmentURL media='first.m4s'/><SegmentURL media='second.m4s'/></SegmentList>",
    );
    assert_eq!(
        source_references(xml.as_bytes()).unwrap(),
        vec![
            PathBuf::from("init.mp4"),
            PathBuf::from("first.m4s"),
            PathBuf::from("second.m4s")
        ]
    );
}

#[test]
fn duration_template_enumerates_the_complete_finite_period() {
    let xml = presentation(
        "<SegmentTemplate timescale='1000' duration='6000' initialization='init-$RepresentationID$.mp4' media='seg-$Number$.m4s'/>",
    );
    assert_eq!(
        source_references(xml.as_bytes()).unwrap(),
        vec![
            PathBuf::from("init-v.mp4"),
            PathBuf::from("seg-1.m4s"),
            PathBuf::from("seg-2.m4s")
        ]
    );
}

#[test]
fn nominal_own_duration_and_explicit_end_number_do_not_invent_a_final_resource() {
    let xml = presentation(
        "<SegmentTemplate timescale='1000' duration='6000' presentationDuration='6001' startNumber='0' endNumber='0' initialization='init.mp4' media='part-$Number$.m4s'/>",
    );
    assert_eq!(
        source_references(xml.as_bytes()).unwrap(),
        [PathBuf::from("init.mp4"), PathBuf::from("part-0.m4s")]
    );
    for invalid in [
        xml.replace("6001", "12001"),
        xml.replace("endNumber='0'", "endNumber='3'"),
        xml.replace("6001", "0"),
    ] {
        assert!(source_references(invalid.as_bytes()).is_err());
    }
}

#[test]
fn segment_base_inventory_keeps_the_whole_indexed_file_once() {
    let xml = presentation(
        "<BaseURL>video.mp4</BaseURL><SegmentBase timescale='1000' indexRange='100-155'><Initialization range='0-99'/></SegmentBase>",
    );
    assert_eq!(
        source_references(xml.as_bytes()).unwrap(),
        vec![PathBuf::from("video.mp4")]
    );
}

#[test]
fn range_only_list_uses_the_inherited_single_media_object() {
    let xml = presentation(
        "<BaseURL>audio/stream.mp4</BaseURL><SegmentList timescale='1000'><Initialization range='0-99'/><SegmentTimeline><S d='6000' r='1'/></SegmentTimeline><SegmentURL mediaRange='100-199'/><SegmentURL mediaRange='200-299'/></SegmentList>",
    );
    assert_eq!(
        source_references(xml.as_bytes()).unwrap(),
        [PathBuf::from("audio/stream.mp4")]
    );
    assert!(
        source_references(
            xml.replace("<BaseURL>audio/stream.mp4</BaseURL>", "")
                .as_bytes()
        )
        .is_err()
    );
    let template = presentation(
        "<BaseURL>ignored.mp4</BaseURL><SegmentTemplate timescale='1000' duration='6000' initialization='init.mp4' media='part-$Number$.m4s'/>",
    );
    assert_eq!(
        source_references(template.as_bytes()).unwrap(),
        [
            PathBuf::from("init.mp4"),
            PathBuf::from("part-1.m4s"),
            PathBuf::from("part-2.m4s")
        ]
    );
}

#[test]
fn ranges_must_be_disjoint_and_addressing_features_cannot_be_silently_ignored() {
    let valid = "<SegmentList timescale='1000'><Initialization sourceURL='media.mp4' range='0-99'/><SegmentTimeline><S t='0' d='6000' r='1'/></SegmentTimeline><SegmentURL media='media.mp4' mediaRange='100-199'/><SegmentURL media='media.mp4' mediaRange='200-299'/></SegmentList>";
    assert_eq!(
        source_references(presentation(valid).as_bytes()).unwrap(),
        [PathBuf::from("media.mp4")]
    );
    for invalid in [
        valid.replace("200-299", "199-299"),
        valid.replace("mediaRange='200-299'", ""),
        valid.replace("mediaRange='100-199'", "mediaRange='99-199'"),
        valid.replace("<SegmentList ", "<SegmentList availabilityTimeOffset='1' "),
        valid.replace("<SegmentURL ", "<SegmentURL index='other.idx' "),
        valid.replace("<S t=", "<S k='2' t="),
    ] {
        assert!(source_references(presentation(&invalid).as_bytes()).is_err());
    }
}

#[test]
fn nominal_addressing_bounds_each_edge_without_rounding_actual_sample_clocks() {
    use addressing::{Nominal, Resource, Segment, check_nominal};
    let nominal = Nominal {
        duration: 6000,
        period: (12, 1),
        delta: 0,
        explicit_presentation: true,
    };
    assert_eq!(nominal.count(1000).unwrap(), 2);
    let first = Segment {
        resource: Resource {
            uri: "one.m4s".into(),
            range: None,
        },
        number: 1,
        time: 10000,
        duration: 6000,
    };
    let second = Segment {
        time: 16000,
        number: 2,
        ..first.clone()
    };
    check_nominal(&first, nominal, 10000, 1000, (10000, 13000, 1000), 0, 2).unwrap();
    check_nominal(&second, nominal, 10000, 1000, (13000, 22000, 1000), 1, 2).unwrap();
    assert!(check_nominal(&first, nominal, 10000, 1000, (10000, 12999, 1000), 0, 2).is_err());
    assert!(check_nominal(&second, nominal, 10000, 1000, (13000, 21999, 1000), 1, 2).is_err());
    let first = Segment { time: 0, ..first };
    let second = Segment {
        time: 6000,
        ..second
    };
    check_nominal(&first, nominal, 0, 1000, (0, 264_192, 44_100), 0, 2).unwrap();
    check_nominal(&second, nominal, 0, 1000, (264_192, 529_200, 44_100), 1, 2).unwrap();
    let longer = Nominal {
        period: (12_000_000_001, 1_000_000_000),
        ..nominal
    };
    assert!(check_nominal(&second, longer, 0, 1000, (264_192, 529_200, 44_100), 1, 2).is_err());
}
