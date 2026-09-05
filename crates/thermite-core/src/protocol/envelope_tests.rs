use super::*;

fn types<'a>(envelope: &'a Envelope<'a>) -> Vec<&'a str> {
    envelope.items.iter().map(|i| i.item_type()).collect()
}

#[test]
fn parses_the_spec_example_with_explicit_lengths() {
    // Verbatim from the envelope spec, minus the BOM/CRLF bytes which are awkward in a literal.
    let input = concat!(
        r#"{"event_id":"9ec79c33ec9942ab8353589fcb2e04dc","dsn":"https://e12d836b15bb49d7bbf99e64295d995b:@sentry.io/42"}"#,
        "\n",
        r#"{"type":"attachment","length":10,"content_type":"text/plain","filename":"hello.txt"}"#,
        "\n",
        "helloworld\n",
        r#"{"type":"event","length":41,"content_type":"application/json"}"#,
        "\n",
        r#"{"message":"hello world","level":"error"}"#,
        "\n",
    );

    let envelope = parse(input.as_bytes()).unwrap();

    assert_eq!(
        envelope.headers.event_id.as_deref(),
        Some("9ec79c33ec9942ab8353589fcb2e04dc")
    );
    assert!(envelope.headers.dsn.is_some());
    assert_eq!(types(&envelope), vec!["attachment", "event"]);
    assert_eq!(envelope.items[0].payload, b"helloworld");
    assert_eq!(
        envelope.items[1].payload,
        br#"{"message":"hello world","level":"error"}"#
    );
}

#[test]
fn parses_implicit_length_terminated_by_newline() {
    let input = concat!(
        r#"{"event_id":"9ec79c33ec9942ab8353589fcb2e04dc"}"#,
        "\n",
        r#"{"type":"attachment"}"#,
        "\n",
        "helloworld\n",
    );

    let envelope = parse(input.as_bytes()).unwrap();

    assert_eq!(types(&envelope), vec!["attachment"]);
    assert_eq!(envelope.items[0].payload, b"helloworld");
}

#[test]
fn payload_with_declared_length_may_contain_newlines() {
    let payload = "line one\nline two";
    let input = format!(
        "{{}}\n{{\"type\":\"event\",\"length\":{}}}\n{payload}\n",
        payload.len()
    );

    let envelope = parse(input.as_bytes()).unwrap();

    assert_eq!(envelope.items.len(), 1);
    assert_eq!(envelope.items[0].payload, payload.as_bytes());
}

#[test]
fn trailing_newline_is_optional() {
    let with = "{}\n{\"type\":\"event\",\"length\":2}\nhi\n";
    let without = "{}\n{\"type\":\"event\",\"length\":2}\nhi";

    for input in [with, without] {
        let envelope = parse(input.as_bytes()).unwrap();
        assert_eq!(envelope.items.len(), 1, "failed for {input:?}");
        assert_eq!(envelope.items[0].payload, b"hi");
    }
}

#[test]
fn eof_right_after_envelope_headers_is_valid_and_yields_no_items() {
    // An SDK flushing an empty envelope must not be treated as an error.
    for input in ["{}", "{}\n"] {
        let envelope = parse(input.as_bytes()).unwrap();
        assert!(envelope.items.is_empty(), "failed for {input:?}");
    }
}

#[test]
fn unknown_item_types_and_header_keys_are_tolerated() {
    let input = concat!(
        "{}\n",
        r#"{"type":"nonsense_from_the_future","length":3,"unknown_key":true}"#,
        "\n",
        "abc\n",
        r#"{"type":"event","length":2}"#,
        "\n",
        "hi\n",
    );

    let envelope = parse(input.as_bytes()).unwrap();

    assert_eq!(types(&envelope), vec!["nonsense_from_the_future", "event"]);
}

#[test]
fn item_missing_a_type_parses_with_an_empty_type() {
    let envelope = parse(b"{}\n{\"length\":2}\nhi\n").unwrap();

    assert_eq!(envelope.items.len(), 1);
    assert_eq!(envelope.items[0].item_type(), "");
}

#[test]
fn empty_input_is_an_error() {
    assert_eq!(parse(b"").unwrap_err(), ParseError::Empty);
    assert_eq!(parse(b"\n").unwrap_err(), ParseError::Empty);
}

#[test]
fn non_json_envelope_header_is_an_error() {
    let err = parse(b"not json\n").unwrap_err();
    assert!(matches!(
        err,
        ParseError::NotJson {
            context: "envelope",
            ..
        }
    ));
}

#[test]
fn non_json_item_header_is_an_error() {
    let err = parse(b"{}\nnot json\npayload\n").unwrap_err();
    assert!(matches!(
        err,
        ParseError::NotJson {
            context: "item",
            ..
        }
    ));
}

#[test]
fn non_utf8_header_is_an_error() {
    assert_eq!(
        parse(&[0xff, 0xfe, b'\n']).unwrap_err(),
        ParseError::NotUtf8("envelope")
    );
}

#[test]
fn declared_length_beyond_the_buffer_is_an_error() {
    let err = parse(b"{}\n{\"type\":\"event\",\"length\":100}\nshort").unwrap_err();
    assert_eq!(
        err,
        ParseError::TruncatedItem {
            declared: 100,
            available: 5
        }
    );
}

#[test]
fn declared_length_not_followed_by_newline_is_an_error() {
    // Declared length of 2 but the payload continues past it, so the byte after is not `\n`.
    let err = parse(b"{}\n{\"type\":\"event\",\"length\":2}\nhi there\n").unwrap_err();
    assert_eq!(err, ParseError::UnterminatedItem);
}

#[test]
fn accepts_exactly_the_item_limit() {
    let mut input = String::from("{}\n");
    for _ in 0..MAX_ITEMS {
        input.push_str("{\"type\":\"event\",\"length\":2}\nhi\n");
    }

    let envelope = parse(input.as_bytes()).unwrap();
    assert_eq!(envelope.items.len(), MAX_ITEMS);
}

#[test]
fn rejects_one_item_past_the_limit() {
    let mut input = String::from("{}\n");
    for _ in 0..MAX_ITEMS + 1 {
        input.push_str("{\"type\":\"event\",\"length\":2}\nhi\n");
    }

    assert_eq!(
        parse(input.as_bytes()).unwrap_err(),
        ParseError::TooManyItems { limit: MAX_ITEMS }
    );
}

#[test]
fn the_item_cap_bounds_a_body_of_minimal_items() {
    // The amplification shape from the review: minimal 4-byte items, which compress to almost
    // nothing on the wire, must not each become an entry in the item list.
    let mut input = String::from("{}\n");
    input.push_str(&"{}\n\n".repeat(50_000));

    assert_eq!(
        parse(input.as_bytes()).unwrap_err(),
        ParseError::TooManyItems { limit: MAX_ITEMS }
    );
}

#[test]
fn a_full_envelope_with_a_trailing_newline_is_still_accepted() {
    // The cap is checked after empty lines are skipped, so the legal trailing newline on an
    // envelope holding exactly MAX_ITEMS items must not be mistaken for another item.
    let mut input = String::from("{}\n");
    for _ in 0..MAX_ITEMS {
        input.push_str("{\"type\":\"event\",\"length\":2}\nhi\n");
    }
    input.push('\n');

    assert_eq!(parse(input.as_bytes()).unwrap().items.len(), MAX_ITEMS);
}

#[test]
fn parse_envelope_headers_reads_the_dsn_without_the_items() {
    let input = concat!(
        r#"{"event_id":"9ec79c33ec9942ab8353589fcb2e04dc","dsn":"http://key@localhost:9000/42"}"#,
        "\n",
        r#"{"type":"event","length":2}"#,
        "\n",
        "hi\n",
    );

    let headers = parse_envelope_headers(input.as_bytes()).unwrap();
    assert_eq!(headers.dsn.as_deref(), Some("http://key@localhost:9000/42"));
}

#[test]
fn parse_envelope_headers_works_on_a_header_only_prefix() {
    // What the pre-auth path actually passes: the first line, with the rest of the body absent.
    let headers = parse_envelope_headers(br#"{"dsn":"http://key@host/1"}"#).unwrap();
    assert_eq!(headers.dsn.as_deref(), Some("http://key@host/1"));
}

#[test]
fn parse_envelope_headers_rejects_a_truncated_header_line() {
    // A header line longer than the pre-auth prefix arrives cut in half. It must fail rather
    // than appear to be a headerless envelope.
    let err = parse_envelope_headers(br#"{"dsn":"http://key@ho"#).unwrap_err();
    assert!(matches!(
        err,
        ParseError::NotJson {
            context: "envelope",
            ..
        }
    ));
}

#[test]
fn parses_a_realistic_session_plus_event_envelope() {
    let input = concat!(
        r#"{"event_id":"9ec79c33ec9942ab8353589fcb2e04dc","sent_at":"2026-07-29T10:00:00Z"}"#,
        "\n",
        r#"{"type":"session","length":2}"#,
        "\n",
        "{}\n",
        r#"{"type":"event","length":23}"#,
        "\n",
        r#"{"level":"error","x":1}"#,
        "\n",
    );

    let envelope = parse(input.as_bytes()).unwrap();
    assert_eq!(types(&envelope), vec!["session", "event"]);
    assert_eq!(
        envelope.headers.sent_at.as_deref(),
        Some("2026-07-29T10:00:00Z")
    );
}
