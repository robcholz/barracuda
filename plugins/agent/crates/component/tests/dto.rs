#![allow(missing_docs)]

use barracuda_agent_component::dto::{FixedStr, FixedStrError, InputRequestIdDto, SessionIdDto};

#[test]
fn fixed_strings_enforce_utf8_byte_capacity_and_json_string_semantics() {
    assert_eq!(FixedStr::<8>::capacity(), 7);
    let text = FixedStr::<8>::new("héllo").expect("six UTF-8 bytes fit");
    assert_eq!(text.as_str(), "héllo");
    assert_eq!(
        serde_json::to_string(&text).expect("serialize"),
        r#""héllo""#
    );
    assert_eq!(
        serde_json::from_str::<FixedStr<8>>(r#""héllo""#).expect("deserialize"),
        text
    );

    assert_eq!(
        FixedStr::<1>::new("").expect("terminator fits").as_str(),
        ""
    );
    assert_eq!(FixedStr::<0>::capacity(), 0);
    assert_eq!(FixedStr::<0>::new(""), Err(FixedStrError::TooLong));
    assert_eq!(FixedStr::<4>::new("four"), Err(FixedStrError::TooLong));
    assert_eq!(FixedStr::<8>::new("a\0b"), Err(FixedStrError::EmbeddedNul));
    assert_eq!(
        serde_json::from_str::<FixedStr<4>>(r#""four""#)
            .expect_err("oversized JSON string")
            .to_string(),
        "string exceeds fixed capacity"
    );
    assert_eq!(
        FixedStrError::TooLong.to_string(),
        "string exceeds fixed capacity"
    );
    assert_eq!(
        FixedStrError::EmbeddedNul.to_string(),
        "string contains a NUL byte"
    );
}

#[test]
fn prefixed_session_and_input_ids_round_trip_and_reject_malformed_wire_values() {
    let session = SessionIdDto::new(42);
    assert_eq!(session.get(), 42);
    assert_eq!(
        serde_json::to_string(&session).expect("serialize"),
        r#""session-42""#
    );
    assert_eq!(
        serde_json::from_str::<SessionIdDto>(r#""session-42""#).expect("deserialize"),
        session
    );

    let request = InputRequestIdDto::new(7);
    assert_eq!(request.get(), 7);
    assert_eq!(
        serde_json::to_string(&request).expect("serialize"),
        r#""input-7""#
    );
    assert_eq!(
        serde_json::from_str::<InputRequestIdDto>(r#""input-7""#).expect("deserialize"),
        request
    );

    for malformed in [r#""42""#, r#""session-x""#, r#""session-4294967296""#] {
        assert!(serde_json::from_str::<SessionIdDto>(malformed).is_err());
    }
    for malformed in [r#""7""#, r#""input-x""#, r#""input-4294967296""#] {
        assert!(serde_json::from_str::<InputRequestIdDto>(malformed).is_err());
    }
}
