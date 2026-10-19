//! Exercise the reviewed FFI output-pointer backport under optimization on Linux.

use glib::prelude::*;

#[test]
fn optimized_variant_string_iterators_return_the_original_strings() {
    for index in 0..256 {
        let words = [
            format!("first-{index}"),
            "middle".to_owned(),
            format!("last-{index}"),
        ];
        let value = words.to_variant();
        let mut forward = value.array_iter_str().unwrap();
        assert_eq!(forward.next(), Some(words[0].as_str()));
        assert_eq!(forward.next(), Some("middle"));
        assert_eq!(forward.next(), Some(words[2].as_str()));
        assert_eq!(forward.next(), None);
        assert_eq!(
            value.array_iter_str().unwrap().last(),
            Some(words[2].as_str())
        );
        let mut backward = value.array_iter_str().unwrap();
        assert_eq!(backward.next_back(), Some(words[2].as_str()));
        assert_eq!(backward.next_back(), Some("middle"));
        assert_eq!(backward.next_back(), Some(words[0].as_str()));
        assert_eq!(backward.next_back(), None);
        assert_eq!(value.array_iter_str().unwrap().nth(1), Some("middle"));
        assert_eq!(value.array_iter_str().unwrap().nth_back(1), Some("middle"));
    }
}
