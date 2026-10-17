//! HLS attribute parsing with valid, malformed, and ambiguous input cases.

use drmr_core::m3u8::{SourceError, parse_attributes};

#[test]
fn quoted_attributes_keep_commas_inside_quotes() {
    let attrs =
        parse_attributes(r#"TYPE=AUDIO,NAME="English, descriptive",URI="audio/a.m3u8?x=1,2""#)
            .unwrap();
    assert_eq!(
        attrs.iter().collect::<Vec<_>>(),
        vec![
            ("TYPE", "AUDIO"),
            ("NAME", "English, descriptive"),
            ("URI", "audio/a.m3u8?x=1,2")
        ]
    );
    assert_eq!(attrs.get("URI"), Some("audio/a.m3u8?x=1,2"));
    assert_eq!(attrs.get("uri"), None);
    assert_eq!(attrs.len(), 3);
    assert!(!attrs.is_empty());
    assert!(parse_attributes("").unwrap().is_empty());
    let japanese =
        parse_attributes(r#"NAME="日本語, 音声",LANGUAGE=ja,BANDWIDTH=1934545"#).unwrap();
    assert_eq!(japanese.get("NAME"), Some("日本語, 音声"));
    assert_eq!(japanese.get("BANDWIDTH"), Some("1934545"));
}

#[test]
fn malformed_lists_are_invalid_attributes() {
    for bad in [
        r#"NAME="unterminated"#,
        "TYPE=AUDIO,TYPE=VIDEO",
        "TYPE=AUDIO,",
        "TYPE=",
        "type=audio",
        "TYPE=AUDIO,,NAME=x",
        "=AUDIO",
        "NAME=\"a\nb\"",
        "BANDWIDTH=1\r",
        ",TYPE=AUDIO",
    ] {
        assert_eq!(
            parse_attributes(bad),
            Err(SourceError::new("invalid-attributes")),
            "{bad:?}"
        );
    }
    assert_eq!(
        SourceError::new("invalid-attributes").token(),
        "invalid-attributes"
    );
    assert_eq!(
        SourceError::new("invalid-attributes").to_string(),
        "invalid-attributes"
    );
}
