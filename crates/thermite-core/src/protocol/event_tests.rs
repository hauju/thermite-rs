use serde_json::json;

use super::*;

fn now() -> DateTime<Utc> {
    Utc.timestamp_opt(1_800_000_000, 0).unwrap()
}

#[test]
fn reads_a_hyphenated_or_simple_event_id() {
    let simple = json!({ "event_id": "9ec79c33ec9942ab8353589fcb2e04dc" });
    let hyphenated = json!({ "event_id": "9ec79c33-ec99-42ab-8353-589fcb2e04dc" });

    assert_eq!(event_id(&simple), event_id(&hyphenated));
}

#[test]
fn generates_an_event_id_when_missing_or_invalid() {
    assert_ne!(event_id(&json!({})), event_id(&json!({})));
    assert_ne!(event_id(&json!({ "event_id": "nonsense" })), Uuid::nil());
}

#[test]
fn parses_timestamp_from_every_shape_sdks_send() {
    let expected = Utc.timestamp_opt(1_799_000_000, 0).unwrap();

    for value in [
        json!(1_799_000_000),
        json!(1_799_000_000.0),
        json!("1799000000"),
        json!("2027-01-03T18:13:20Z"),
        json!("2027-01-03T18:13:20+00:00"),
        json!("2027-01-03T18:13:20"),
    ] {
        let event = json!({ "timestamp": value });
        assert_eq!(timestamp(&event, now()), expected, "failed for {value}");
    }
}

#[test]
fn falls_back_to_received_at_for_a_missing_or_broken_timestamp() {
    for value in [json!(null), json!("not a date"), json!({})] {
        let event = json!({ "timestamp": value });
        assert_eq!(timestamp(&event, now()), now(), "failed for {value}");
    }
    assert_eq!(timestamp(&json!({}), now()), now());
}

#[test]
fn clamps_a_timestamp_from_a_clock_running_fast() {
    let far_future = json!({ "timestamp": now().timestamp() + 86_400 });
    assert_eq!(timestamp(&far_future, now()), now());

    // An hour of skew is tolerated rather than clamped.
    let slightly_ahead = json!({ "timestamp": now().timestamp() + 600 });
    assert!(timestamp(&slightly_ahead, now()) > now());
}

#[test]
fn honours_a_recent_past_timestamp_but_not_an_ancient_one() {
    // A retry buffer flushing hours or days late is legitimate and kept.
    let five_days = now() - chrono::Duration::days(5);
    let event = json!({ "timestamp": five_days.timestamp() });
    assert_eq!(timestamp(&event, now()), five_days);

    // Beyond 30 days it is treated like a broken clock: every honoured hour of backdating
    // mints a permanent event_counts bucket, so the horizon must be finite.
    let ancient = json!({ "timestamp": 1_600_000_000 });
    assert_eq!(timestamp(&ancient, now()), now());
}

#[test]
fn fingerprint_parts_are_capped_in_count_and_length() {
    // Both are amplifier bounds, not preferences: every `{{ default }}` part costs a regex
    // scan plus a ~1 KiB string, held twice while the key is joined.
    let many: Vec<Value> = (0..1000).map(|i| json!(format!("part-{i}"))).collect();
    let event = json!({ "fingerprint": many });
    let parts = fingerprint(&event).unwrap();
    assert_eq!(parts.len(), 32);
    assert_eq!(parts[0], "part-0", "the kept parts are the first ones");

    let long = "x".repeat(5000);
    let event = json!({ "fingerprint": [long] });
    assert_eq!(fingerprint(&event).unwrap()[0].chars().count(), 256);
}

#[test]
fn level_defaults_to_error_and_rejects_unknown_values() {
    assert_eq!(level(&json!({})), "error");
    assert_eq!(level(&json!({ "level": "warning" })), "warning");
    assert_eq!(level(&json!({ "level": "critical" })), "error");
    assert_eq!(level(&json!({ "level": 3 })), "error");
}

#[test]
fn accepts_all_three_exception_container_shapes() {
    let wrapped = json!({ "exception": { "values": [{ "type": "A", "value": "x" }] } });
    let bare_list = json!({ "exception": [{ "type": "A", "value": "x" }] });
    let bare_object = json!({ "exception": { "type": "A", "value": "x" } });

    for event in [wrapped, bare_list, bare_object] {
        assert_eq!(
            type_and_value(&event),
            ("A".to_string(), "x".to_string()),
            "failed for {event}"
        );
    }
}

#[test]
fn uses_the_last_exception_in_a_chain() {
    // Sentry orders the chain oldest-first, so the last entry is the one actually raised.
    let event = json!({
        "exception": { "values": [
            { "type": "ValueError", "value": "inner" },
            { "type": "RuntimeError", "value": "outer" },
        ]}
    });

    assert_eq!(
        type_and_value(&event),
        ("RuntimeError".to_string(), "outer".to_string())
    );
}

#[test]
fn defaults_a_missing_exception_type_to_error() {
    let event = json!({ "exception": { "values": [{ "value": "boom" }] } });
    assert_eq!(
        type_and_value(&event),
        ("Error".to_string(), "boom".to_string())
    );
}

#[test]
fn an_empty_exception_list_falls_through_to_the_log_message() {
    let event = json!({ "exception": { "values": [] }, "message": "hello" });
    assert_eq!(
        type_and_value(&event),
        ("Log Message".to_string(), "hello".to_string())
    );
}

#[test]
fn synthetic_exceptions_are_identified_by_their_crash_function() {
    let event = json!({
        "exception": { "values": [{
            "type": "Error",
            "value": "whatever",
            "mechanism": { "synthetic": true },
            "stacktrace": { "frames": [
                { "function": "outer", "in_app": true },
                { "function": "handle_request", "in_app": true },
            ]}
        }]}
    });

    assert_eq!(
        type_and_value(&event),
        ("handle_request".to_string(), String::new())
    );
}

#[test]
fn a_synthetic_exception_without_frames_is_unknown() {
    let event = json!({
        "exception": { "values": [{
            "type": "Error",
            "mechanism": { "synthetic": true },
        }]}
    });

    assert_eq!(
        type_and_value(&event),
        ("<unknown>".to_string(), String::new())
    );
}

#[test]
fn walks_the_log_message_fallback_chain_in_order() {
    let cases = [
        (json!({ "logentry": { "message": "a" } }), "a"),
        (json!({ "logentry": { "formatted": "b" } }), "b"),
        (json!({ "message": { "message": "c" } }), "c"),
        (json!({ "message": { "formatted": "d" } }), "d"),
        (json!({ "message": "e" }), "e"),
    ];

    for (event, expected) in cases {
        assert_eq!(
            type_and_value(&event),
            ("Log Message".to_string(), expected.to_string()),
            "failed for {event}"
        );
    }
}

#[test]
fn message_prefers_logentry_over_message() {
    let event = json!({ "logentry": { "message": "structured" }, "message": "raw" });
    assert_eq!(type_and_value(&event).1, "structured");
}

#[test]
fn an_event_with_no_message_at_all_is_labelled() {
    assert_eq!(
        type_and_value(&json!({})),
        ("Log Message".to_string(), "<no log message>".to_string())
    );
}

#[test]
fn only_the_first_line_of_a_log_message_is_used() {
    let event = json!({ "message": "first line\nsecond line" });
    assert_eq!(type_and_value(&event).1, "first line");
}

#[test]
fn long_types_and_values_are_trimmed() {
    let event = json!({
        "exception": { "values": [{
            "type": "T".repeat(200),
            "value": "V".repeat(2000),
        }]}
    });

    let (exception_type, value) = type_and_value(&event);
    assert_eq!(exception_type.len(), MAX_TYPE_LEN);
    assert_eq!(value.len(), MAX_VALUE_LEN);
}

#[test]
fn title_omits_an_empty_value() {
    assert_eq!(title("KeyError", "missing"), "KeyError: missing");
    assert_eq!(title("KeyError", ""), "KeyError");
    assert_eq!(
        title("KeyError", "line one\nline two"),
        "KeyError: line one"
    );
}

#[test]
fn crash_location_prefers_the_innermost_in_app_frame() {
    let event = json!({
        "exception": { "values": [{ "stacktrace": { "frames": [
            { "function": "main", "filename": "main.rs", "in_app": true },
            { "function": "handler", "filename": "handler.rs", "in_app": true },
            { "function": "poll", "filename": "tokio.rs", "in_app": false },
        ]}}]}
    });

    assert_eq!(
        crash_location(&event),
        (Some("handler.rs".into()), Some("handler".into()))
    );
    assert_eq!(culprit(&event).as_deref(), Some("handler.rs in handler"));
}

#[test]
fn crash_location_falls_back_to_the_innermost_usable_frame() {
    let event = json!({
        "exception": { "values": [{ "stacktrace": { "frames": [
            { "function": "main", "filename": "main.rs" },
            { "function": "poll", "filename": "tokio.rs" },
        ]}}]}
    });

    assert_eq!(
        crash_location(&event),
        (Some("tokio.rs".into()), Some("poll".into()))
    );
}

#[test]
fn crash_location_skips_frames_without_a_usable_function() {
    let event = json!({
        "exception": { "values": [{ "stacktrace": { "frames": [
            { "function": "real_frame", "filename": "a.rs" },
            { "function": "<redacted>", "filename": "b.rs" },
            { "function": "<unknown>", "filename": "c.rs" },
            { "filename": "d.rs" },
        ]}}]}
    });

    assert_eq!(
        crash_location(&event),
        (Some("a.rs".into()), Some("real_frame".into()))
    );
}

#[test]
fn crash_location_reads_abs_path_when_filename_is_absent() {
    let event = json!({
        "exception": { "values": [{ "stacktrace": { "frames": [
            { "function": "f", "abs_path": "/src/a.rs" },
        ]}}]}
    });

    assert_eq!(crash_location(&event).0.as_deref(), Some("/src/a.rs"));
}

#[test]
fn crash_location_reads_event_level_and_single_thread_stacktraces() {
    let event_level = json!({
        "stacktrace": { "frames": [{ "function": "f", "filename": "a.rs" }] }
    });
    assert_eq!(crash_location(&event_level).1.as_deref(), Some("f"));

    let threaded = json!({
        "threads": { "values": [
            { "stacktrace": { "frames": [{ "function": "g", "filename": "b.rs" }] } }
        ]}
    });
    assert_eq!(crash_location(&threaded).1.as_deref(), Some("g"));

    // With more than one thread there is no single crashing stack to pick.
    let multi = json!({
        "threads": { "values": [
            { "stacktrace": { "frames": [{ "function": "g" }] } },
            { "stacktrace": { "frames": [{ "function": "h" }] } },
        ]}
    });
    assert_eq!(crash_location(&multi), (None, None));
}

#[test]
fn culprit_falls_back_to_the_transaction() {
    let event = json!({ "transaction": "GET /users" });
    assert_eq!(culprit(&event).as_deref(), Some("GET /users"));
    assert_eq!(culprit(&json!({})), None);
}

#[test]
fn reads_fingerprints_and_ignores_empty_ones() {
    assert_eq!(
        fingerprint(&json!({ "fingerprint": ["my-group"] })),
        Some(vec!["my-group".to_string()])
    );
    // Numbers appear in the wild; Sentry stringifies them.
    assert_eq!(
        fingerprint(&json!({ "fingerprint": ["a", 42] })),
        Some(vec!["a".to_string(), "42".to_string()])
    );
    assert_eq!(fingerprint(&json!({ "fingerprint": [] })), None);
    assert_eq!(fingerprint(&json!({})), None);
    assert_eq!(fingerprint(&json!({ "fingerprint": "not-a-list" })), None);
}

#[test]
fn str_field_treats_blank_as_absent() {
    let event = json!({ "release": "1.0.0", "environment": "  ", "dist": "" });

    assert_eq!(str_field(&event, "release"), Some("1.0.0"));
    assert_eq!(str_field(&event, "environment"), None);
    assert_eq!(str_field(&event, "dist"), None);
    assert_eq!(str_field(&event, "missing"), None);
}

#[test]
fn tags_accepts_every_shape_sdks_send() {
    let as_map = json!({ "tags": { "browser": "firefox", "attempts": 3, "cached": true } });
    let as_pairs = json!({ "tags": [["browser", "firefox"], ["attempts", 3], ["cached", true]] });
    let as_objects = json!({ "tags": [
        { "key": "browser", "value": "firefox" },
        { "key": "attempts", "value": 3 },
        { "key": "cached", "value": true },
    ] });

    for event in [&as_map, &as_pairs, &as_objects] {
        let mut tags = tags(event);
        tags.sort();
        assert_eq!(
            tags,
            vec![
                ("attempts".into(), "3".into()),
                ("browser".into(), "firefox".into()),
                ("cached".into(), "true".into()),
            ]
        );
    }
}

#[test]
fn promoted_fields_are_synthesized_as_tags_and_shadow_sdk_duplicates() {
    let event = json!({
        "environment": "production",
        "release": "abc123",
        "tags": { "environment": "spoofed", "browser": "firefox" },
    });

    let tags = tags(&event);
    assert_eq!(
        tags,
        vec![
            ("environment".into(), "production".into()),
            ("release".into(), "abc123".into()),
            ("browser".into(), "firefox".into()),
        ]
    );
}

#[test]
fn user_identity_prefers_the_strongest_identifier() {
    // SDKs usually send ip_address alongside id; id must win.
    let event = json!({ "user": { "id": 42, "ip_address": "10.0.0.5" } });
    assert_eq!(user_key(&event).as_deref(), Some("id:42"));

    let event = json!({ "user": { "email": "a@b.c", "ip_address": "10.0.0.5" } });
    assert_eq!(user_key(&event).as_deref(), Some("email:a@b.c"));

    // An IP alone is not an identity: it would mint one permanent tag row per visitor IP
    // and is the one identifier retention could never erase.
    let event = json!({ "user": { "ip_address": "10.0.0.5" } });
    assert_eq!(user_key(&event), None);

    assert_eq!(user_key(&json!({})), None);
    assert_eq!(user_key(&json!({ "user": {} })), None);
    assert_eq!(user_key(&json!({ "user": { "id": "  " } })), None);

    // And the identity is synthesized as a tag, which is what the counts read.
    let event = json!({ "user": { "id": 42 } });
    assert_eq!(tags(&event), vec![("user".into(), "id:42".into())]);
}

#[test]
fn tags_skip_junk_and_are_capped() {
    let event = json!({ "tags": {
        "": "no key",
        "blank": "   ",
        "nested": { "not": "a scalar" },
        "list": [1, 2],
        "null": null,
        "ok": "kept",
    } });
    assert_eq!(tags(&event), vec![("ok".into(), "kept".into())]);

    let long = "x".repeat(500);
    let event = json!({ "tags": { "key": long } });
    assert_eq!(tags(&event)[0].1.chars().count(), 200);

    let many: serde_json::Map<String, Value> = (0..100)
        .map(|i| (format!("key{i:03}"), json!("v")))
        .collect();
    let event = json!({ "environment": "production", "tags": many });
    let tags = tags(&event);
    assert_eq!(tags.len(), 50);
    // The synthesized tag survives the cap; SDK spam is what gets cut.
    assert_eq!(tags[0], ("environment".into(), "production".into()));
}
