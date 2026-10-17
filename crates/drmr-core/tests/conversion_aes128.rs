use drmr_core::conversion::{
    Aes128Key, ConversionError, HlsInputKeys, HlsOutputKey, SegmentIv, decrypt_segment,
    encrypt_segment,
};
use std::fs;

const KEY: &str = "2b7e151628aed2a6abf7158809cf4f3c";
const IV: &str = "000102030405060708090a0b0c0d0e0f";
#[cfg(unix)]
#[test]
fn nist_cbc_vector_and_rustcrypto_pkcs7_vector_match_exactly() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let result = directory.path().join("cipher");
    let restored = directory.path().join("restored");
    let plaintext = hex::decode(concat!(
        "6bc1bee22e409f96e93d7e117393172a",
        "ae2d8a571e03ac9c9eb76fac45af8e51",
        "30c81c46a35ce411e5fbc1191a0a52ef",
        "f69f2445df4f9b17ad2b417be66c3710"
    ))
    .unwrap();
    fs::write(&source, &plaintext).unwrap();
    encrypt_segment(
        &source,
        &result,
        &Aes128Key::from_hex(KEY).unwrap(),
        &SegmentIv::from_hex(IV).unwrap(),
    )
    .unwrap();
    assert_eq!(
        &fs::read(&result).unwrap()[..64],
        hex::decode(concat!(
            "7649abac8119b246cee98e9b12e9197d",
            "5086cb9b507219ee95db113a917678b2",
            "73bed6b8e3c1743b7116e69e22229516",
            "3ff1caa1681fac09120eca307586e1a7"
        ))
        .unwrap()
    );
    decrypt_segment(
        &result,
        &restored,
        &Aes128Key::from_hex(KEY).unwrap(),
        &SegmentIv::from_hex(IV).unwrap(),
    )
    .unwrap();
    assert_eq!(fs::read(&restored).unwrap(), plaintext);
    let source = directory.path().join("message");
    let result = directory.path().join("message-cipher");
    fs::write(&source, b"hello world! this is my plaintext.").unwrap();
    encrypt_segment(
        &source,
        &result,
        &Aes128Key::from_hex(&"42".repeat(16)).unwrap(),
        &SegmentIv::from_hex(&"24".repeat(16)).unwrap(),
    )
    .unwrap();
    assert_eq!(
        hex::encode(fs::read(&result).unwrap()),
        "c7fe247ef97b21f07cbdd26cb5d346bfd27867cb00d9486723e159978fb9a5f914cfb228a710de4171e396e7b6cf859e"
    );
}
#[cfg(unix)]
#[test]
fn streaming_chunk_boundaries_preserve_cbc_chaining() {
    use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
    let directory = tempfile::tempdir().unwrap();
    for length in [0, 1, 15, 16, 17, 65535, 65536, 65537, 200_003] {
        let plaintext: Vec<_> = (0..length)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect();
        let source = directory.path().join(format!("source-{length}"));
        let encrypted = directory.path().join(format!("encrypted-{length}"));
        let restored = directory.path().join(format!("restored-{length}"));
        fs::write(&source, &plaintext).unwrap();
        let key = Aes128Key::from_hex(KEY).unwrap();
        let iv = SegmentIv::from_hex(IV).unwrap();
        encrypt_segment(&source, &encrypted, &key, &iv).unwrap();
        decrypt_segment(&encrypted, &restored, &key, &iv).unwrap();
        assert_eq!(fs::read(&restored).unwrap(), plaintext);
        let expected = cbc::Encryptor::<aes::Aes128>::new_from_slices(
            &hex::decode(KEY).unwrap(),
            &hex::decode(IV).unwrap(),
        )
        .unwrap()
        .encrypt_padded_vec_mut::<Pkcs7>(&plaintext);
        assert_eq!(fs::read(&encrypted).unwrap(), expected);
    }
}
#[cfg(unix)]
#[test]
fn invalid_padding_and_truncation_never_publish_plaintext() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("cipher");
    let output = directory.path().join("output");
    let key = Aes128Key::from_hex(KEY).unwrap();
    let iv = SegmentIv::from_hex(IV).unwrap();
    // The unpadded NIST plaintext ends in 0x2a, which cannot be valid PKCS#7 padding.
    for bytes in [
        vec![],
        vec![0; 15],
        hex::decode("7649abac8119b246cee98e9b12e9197d").unwrap(),
    ] {
        fs::write(&input, bytes).unwrap();
        assert!(decrypt_segment(&input, &output, &key, &iv).is_err());
        assert!(!output.exists());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
#[test]
fn key_roles_and_uris_validate_without_exposing_material() {
    assert_eq!(
        Aes128Key::from_hex("SECRET").unwrap_err(),
        ConversionError("invalid-aes128-key")
    );
    let keys = HlsInputKeys::new(vec![(
        "https://SECRET/key".into(),
        Aes128Key::from_hex(KEY).unwrap(),
    )])
    .unwrap();
    assert!(!format!("{keys:?}").contains("SECRET"));
    assert!(!format!("{keys:?}").contains(KEY));
    assert!(
        HlsInputKeys::new(vec![
            ("same".into(), Aes128Key::from_hex(KEY).unwrap()),
            ("same".into(), Aes128Key::from_hex(KEY).unwrap())
        ])
        .is_err()
    );
    assert!(HlsOutputKey::new("bad\nuri", Aes128Key::from_hex(KEY).unwrap()).is_err());
    assert!(
        !format!(
            "{:?}",
            HlsOutputKey::new("SECRET", Aes128Key::from_hex(KEY).unwrap()).unwrap()
        )
        .contains("SECRET")
    );
    assert!(!format!("{:?}", SegmentIv::for_sequence(u64::MAX)).contains("ffff"));
}
