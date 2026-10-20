use super::*;
fn output_key() -> OutputKey {
    OutputKey::from_hex(
        "000102030405060708090a0b0c0d0e0f",
        "101112131415161718191a1b1c1d1e1f",
    )
    .unwrap()
}
fn boxed(key: &OutputKey) -> Vec<u8> {
    let mut bytes = vec![0, 0, 0, 52];
    bytes.extend_from_slice(b"pssh");
    bytes.extend_from_slice(&[1, 0, 0, 0]);
    bytes.extend_from_slice(&SYSTEM);
    bytes.extend_from_slice(&1_u32.to_be_bytes());
    bytes.extend_from_slice(&key.0.id);
    bytes.extend_from_slice(&0_u32.to_be_bytes());
    bytes
}
fn descriptors(target: TargetProtection, encoded: &str) -> String {
    format!(
        r#"<ContentProtection schemeIdUri="{COMMON}" value="{}" c:default_KID="00010203-0405-0607-0809-0a0b0c0d0e0f"/><ContentProtection schemeIdUri="{URI}"><c:pssh>{encoded}</c:pssh></ContentProtection>"#,
        target.as_str()
    )
}
fn document(descriptors: &str) -> String {
    format!(
        r#"<MPD xmlns="{DASH}" xmlns:c="{CENC}"><Period><AdaptationSet>{descriptors}<Representation/></AdaptationSet></Period></MPD>"#
    )
}
#[test]
fn common_system_records_are_bound_to_one_declared_output_key_for_cenc_and_cbcs() {
    let key = output_key();
    let bytes = boxed(&key);
    let encoded = STANDARD.encode(&bytes);
    for target in [TargetProtection::Cenc, TargetProtection::Cbcs] {
        let source = document(&descriptors(target, &encoded));
        dash(&source, target, Some(&key)).unwrap();
        let wrapped = source.replace(
            &encoded,
            &format!("\n {}\n{} \n", &encoded[..24], &encoded[24..]),
        );
        dash(&wrapped, target, Some(&key)).unwrap();
        assert!(dash(&source, target, None).is_err());
        assert!(dash(&source, TargetProtection::Clear, Some(&key)).is_err());
        assert!(dash(&source, TargetProtection::HlsAes128, Some(&key)).is_err());
        let other = if target == TargetProtection::Cenc {
            TargetProtection::Cbcs
        } else {
            TargetProtection::Cenc
        };
        assert!(dash(&source, other, Some(&key)).is_err());
    }
    dash(&document(""), TargetProtection::Clear, None).unwrap();
}
#[test]
fn malformed_common_boxes_never_become_accepted_provider_metadata() {
    let key = output_key();
    let original = boxed(&key);
    for at in [0, 4, 8, 9, 12, 28, 32, 48] {
        let mut changed = original.clone();
        changed[at] ^= 1;
        let source = document(&descriptors(
            TargetProtection::Cenc,
            &STANDARD.encode(changed),
        ));
        assert!(
            dash(&source, TargetProtection::Cenc, Some(&key)).is_err(),
            "mutated box field {at} must be refused"
        );
    }
    let mut trailing = original.clone();
    trailing.push(0);
    let mut repeated = original.clone();
    repeated.extend_from_slice(&original);
    let mut data = original.clone();
    data[3] = 53;
    data[51] = 1;
    data.push(0);
    let mut duplicate_kid = original.clone();
    duplicate_kid[3] = 68;
    duplicate_kid[31] = 2;
    duplicate_kid.splice(48..48, key.0.id);
    for changed in [trailing, repeated, data, duplicate_kid] {
        assert!(
            dash(
                &document(&descriptors(
                    TargetProtection::Cenc,
                    &STANDARD.encode(changed)
                )),
                TargetProtection::Cenc,
                Some(&key)
            )
            .is_err()
        );
    }
}
#[test]
fn provider_unknown_duplicate_namespace_and_declaration_changes_are_refused() {
    let key = output_key();
    let encoded = STANDARD.encode(boxed(&key));
    let descriptor = descriptors(TargetProtection::Cenc, &encoded);
    let common = descriptor
        .find(&format!(r#"<ContentProtection schemeIdUri="{URI}""#))
        .unwrap();
    let scheme = &descriptor[..common];
    let record = &descriptor[common..];
    for changed in [
        format!("{descriptor}{record}"),
        format!("{scheme}{descriptor}"),
        descriptor.replace(URI, "urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed"),
        descriptor.replace(URI, "urn:uuid:00000000-0000-0000-0000-000000000000"),
        descriptor.replace(
            "00010203-0405-0607-0809-0a0b0c0d0e0f",
            "10010203-0405-0607-0809-0a0b0c0d0e0f",
        ),
        descriptor.replace("c:pssh", "pssh"),
        descriptor.replace("c:default_KID", "default_KID"),
        descriptor.replace("<c:pssh>", "<c:pssh extra=\"private\">"),
        descriptor.replace("<c:pssh>", "<c:pssh><c:pro/>"),
        descriptor.replace("</c:pssh>", "</c:pssh><c:pssh/>"),
        descriptor.replace("<c:pssh>", "unexpected<c:pssh>"),
        descriptor.replace(&encoded, "AAAA<!-- split -->BBBB"),
        descriptor.replace("value=\"cenc\"", "value=\"cenc\" extra=\"private\""),
    ] {
        assert!(dash(&document(&changed), TargetProtection::Cenc, Some(&key)).is_err());
    }
    let wrong_scope = format!(r#"<MPD xmlns="{DASH}" xmlns:c="{CENC}">{descriptor}</MPD>"#);
    assert!(dash(&wrong_scope, TargetProtection::Cenc, Some(&key)).is_err());
}
