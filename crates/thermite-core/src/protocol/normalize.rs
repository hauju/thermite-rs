//! Message normalization for grouping.
//!
//! Ported from Sentry's `grouping/parameterization.py` and `grouping/strategies/message.py`
//! (as vendored by Bugsink). Variable parts of a message are replaced with placeholders so that
//! `Connection to 10.0.0.5 failed` and `Connection to 10.0.0.7 failed` group together. Without
//! this, any message carrying an id, timestamp or count produces a new issue per occurrence and
//! the issue list becomes unusable.
//!
//! Two deliberate deviations from the Python source:
//!
//! 1. Patterns are written out in compact form rather than using extended (`(?x)`) mode. Rust's
//!    verbose mode treats whitespace and `#` inside character classes differently from Python's,
//!    and the email pattern contains a literal `#`.
//! 2. `hostname` uses one case-insensitive TLD list instead of separate upper- and lower-case
//!    lists, so it also matches mixed case like `Example.Com`. Strictly more permissive, which for
//!    normalization is harmless.
//!
//! The `quoted_str` and `bool` patterns use a `=` lookbehind upstream, which Rust's `regex` does
//! not support. They instead consume the `=` and re-emit it, which avoids pulling in a
//! backtracking engine that could go quadratic on hostile input.

use std::sync::LazyLock;

use regex::{Captures, Regex, RegexBuilder};

/// `(name, pattern, consumes_leading_equals)`, in preference order — the first alternative that
/// matches at a given position wins, so ordering is behaviour, not style.
const PATTERNS: &[(&str, &str, bool)] = &[
    (
        "email",
        r"[a-zA-Z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-zA-Z0-9-]+(?:\.[a-zA-Z0-9-]+)*",
        false,
    ),
    ("url", r"\b(wss?|https?|ftp)://[^\s/$.?#].[^\s]*", false),
    (
        "hostname",
        // Top 100 TLDs; the full list is thousands long.
        r"\b([a-zA-Z0-9\-]{1,63}\.)+?(?i:com|net|org|jp|de|uk|fr|br|it|ru|es|me|gov|pl|ca|au|cn|co|in|nl|edu|info|eu|ch|id|at|kr|cz|mx|be|tv|se|tr|tw|al|ua|ir|vn|cl|sk|ly|cc|to|no|fi|us|pt|dk|ar|hu|tk|gr|il|news|ro|my|biz|ie|za|nz|sg|ee|th|io|xyz|pe|bg|hk|rs|lt|link|ph|club|si|site|mobi|by|cat|wiki|la|ga|xxx|cf|hr|ng|jobs|online|kz|ug|gq|ae|is|lv|pro|fm|tips|ms|sa|app)\b",
        false,
    ),
    (
        "ip",
        // Deviation from upstream: the alternatives are ordered most-specific first. Upstream lists
        // the `h:{1,7}:` catch-all third, and because alternation is preference-ordered it wins on
        // `2001:db8::8a2e:370:7334` after consuming only `2001:db8::`, leaving the rest to be
        // chewed up as separate `<int>`s. Two different addresses then normalize differently, which
        // is exactly what this pass exists to prevent. Same alternatives, reordered.
        concat!(
            // Full eight-group form.
            r"(([0-9a-fA-F]{1,4}:){7,7}[0-9a-fA-F]{1,4}",
            // IPv4-mapped and IPv4-embedded forms, before anything that could match their prefix.
            r"|::(ffff(:0{1,4}){0,1}:){0,1}((25[0-5]|(2[0-4]|1{0,1}[0-9]){0,1}[0-9])\.){3,3}(25[0-5]|(2[0-4]|1{0,1}[0-9]){0,1}[0-9])",
            r"|([0-9a-fA-F]{1,4}:){1,4}:((25[0-5]|(2[0-4]|1{0,1}[0-9]){0,1}[0-9])\.){3,3}(25[0-5]|(2[0-4]|1{0,1}[0-9]){0,1}[0-9])\b",
            // Link-local with a zone index, before the generic compressed forms.
            r"|fe80:(:[0-9a-fA-F]{0,4}){0,4}%[0-9a-zA-Z]{1,}",
            // Compressed forms, ordered by how much they can consume to the right of the `::`.
            r"|([0-9a-fA-F]{1,4}:){1,2}(:[0-9a-fA-F]{1,4}){1,5}",
            r"|([0-9a-fA-F]{1,4}:){1,3}(:[0-9a-fA-F]{1,4}){1,4}",
            r"|([0-9a-fA-F]{1,4}:){1,4}(:[0-9a-fA-F]{1,4}){1,3}",
            r"|([0-9a-fA-F]{1,4}:){1,5}(:[0-9a-fA-F]{1,4}){1,2}",
            r"|([0-9a-fA-F]{1,4}:){1,6}:[0-9a-fA-F]{1,4}",
            r"|[0-9a-fA-F]{1,4}:((:[0-9a-fA-F]{1,4}){1,6})",
            // Trailing `::` and leading `::` catch-alls.
            r"|([0-9a-fA-F]{1,4}:){1,7}:",
            r"|:((:[0-9a-fA-F]{1,4}){1,7}|:)",
            // Plain IPv4.
            r"|(\b((25[0-5]|(2[0-4]|1{0,1}[0-9]){0,1}[0-9])\.){3,3}(25[0-5]|(2[0-4]|1{0,1}[0-9]){0,1}[0-9])\b))",
        ),
        false,
    ),
    (
        "uuid",
        r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b",
        false,
    ),
    ("sha1", r"\b[0-9a-fA-F]{40}\b", false),
    ("md5", r"\b[0-9a-fA-F]{32}\b", false),
    (
        "date",
        concat!(
            // RFC822, RFC1123, RFC1123Z
            r"((?:Sun|Mon|Tue|Wed|Thu|Fri|Sat),\s\d{1,2}\s(?:Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)\s\d{2,4}\s\d{1,2}:\d{1,2}(:\d{1,2})?\s([-\+][\d]{2}[0-5][\d]|(?:UT|GMT|(?:E|C|M|P)(?:ST|DT)|[A-IK-Z])))",
            // "Mon Jan 02, 1999", "Jan 02, 1999"
            r"|(((?:Sun|Mon|Tue|Wed|Thu|Fri|Sat)\s)?(?:Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)\s[0-3]\d,\s\d{2,4})",
            // RFC850
            r"|((?:Sunday|Monday|Tuesday|Wednesday|Thursday|Friday|Saturday),\s\d{2}-(?:Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)-\d{2}\s\d{2}:\d{2}:\d{2}\s(?:UT|GMT|(?:E|C|M|P)(?:ST|DT)|[A-IK-Z]))",
            // RFC3339, RFC3339Nano
            r"|(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?Z?([+-]?\d{2}:\d{2})?)",
            // Datetime
            r"|(\d{4}-?[01]\d-?[0-3]\d\s[0-2]\d:[0-5]\d:[0-5]\d)(\.\d+)?",
            // Kitchen
            r"|(\d{1,2}:\d{2}(:\d{2})?(?: [aApP][Mm])?)",
            // Date
            r"|(\d{4}-[01]\d-[0-3]\d)",
            // Time
            r"|([0-2]\d:[0-5]\d:[0-5]\d)",
            // Older ISO-ish formats
            r"|((\d{4}-[01]\d-[0-3]\dT[0-2]\d:[0-5]\d:[0-5]\d\.\d+([+-][0-2]\d:[0-5]\d|Z))",
            r"|(\d{4}-[01]\d-[0-3]\dT[0-2]\d:[0-5]\d:[0-5]\d([+-][0-2]\d:[0-5]\d|Z))",
            r"|(\d{4}-[01]\d-[0-3]\dT[0-2]\d:[0-5]\d([+-][0-2]\d:[0-5]\d|Z)))",
            // "Mon Jan 2 15:04:05 2006"
            r"|(\b(?:(Sun|Mon|Tue|Wed|Thu|Fri|Sat)\s+)?(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)\s+([\d]{1,2})\s+([\d]{2}:[\d]{2}:[\d]{2})\s+[\d]{4})",
            // "Mon, 02 Jan 2006 15:04:05 MST"
            r"|(\b(?:(Sun|Mon|Tue|Wed|Thu|Fri|Sat),\s+)?(0[1-9]|[1-2]?[\d]|3[01])\s+(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)\s+(19[\d]{2}|[2-9][\d]{3})\s+(2[0-3]|[0-1][\d]):([0-5][\d])(?::(60|[0-5][\d]))?\s+([-\+][\d]{2}[0-5][\d]|(?:UT|GMT|(?:E|C|M|P)(?:ST|DT)|[A-IK-Z])))",
            // Python repr
            r"|(datetime\.datetime\(.*?\))",
        ),
        false,
    ),
    // Deviation from upstream: upstream is `\b(\d+ms) | (\d(\.\d+)?s)\b`, where the seconds branch
    // takes a *single* digit, so `30s` matches only `0s` and normalizes to `3<duration>`. `30s` and
    // `45s` then land in different groups. Anchored on both sides and allowing multiple digits.
    ("duration", r"\b(\d+ms|\d+(\.\d+)?s)\b", false),
    ("hex", r"\b0[xX][0-9a-fA-F]+\b", false),
    ("float", r"-\d+\.\d+\b|\b\d+\.\d+\b", false),
    ("int", r"-\d+\b|\b\d+\b", false),
    // Only the value half of a `key=value` pair, so arbitrary quoted strings survive.
    ("quoted_str", r#"'[^']+'|"[^"]+""#, true),
    ("bool", r"True|true|False|false", true),
];

static PARAMETERIZER: LazyLock<Regex> = LazyLock::new(|| {
    let combined = PATTERNS
        .iter()
        .map(|(name, pattern, needs_equals)| {
            let equals = if *needs_equals { "=" } else { "" };
            format!("{equals}(?P<{name}>{pattern})")
        })
        .collect::<Vec<_>>()
        .join("|");

    RegexBuilder::new(&combined)
        // The combined pattern is large (the TLD and date alternations dominate); the default
        // 10 MiB compile budget is not obviously enough.
        .size_limit(32 * 1024 * 1024)
        .build()
        .expect("parameterization regex is malformed")
});

/// Replaces the variable parts of `message` with placeholders and trims it to two lines.
pub fn normalize_message_for_grouping(message: &str) -> String {
    let mut trimmed = message
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(2)
        .collect::<Vec<_>>()
        .join("\n");

    // Signals that content was dropped, so a long message and its first two lines don't collide.
    if trimmed != message {
        trimmed.push_str("...");
    }

    parameterize(&trimmed)
}

fn parameterize(input: &str) -> String {
    PARAMETERIZER
        .replace_all(input, |caps: &Captures<'_>| {
            for (name, _, needs_equals) in PATTERNS {
                if caps.name(name).is_some() {
                    return if *needs_equals {
                        format!("=<{name}>")
                    } else {
                        format!("<{name}>")
                    };
                }
            }
            // Unreachable: a match implies one of the named alternatives participated.
            String::new()
        })
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regex_compiles() {
        LazyLock::force(&PARAMETERIZER);
    }

    #[test]
    fn replaces_ipv4_addresses() {
        assert_eq!(
            parameterize("Connection to 10.0.0.5 failed"),
            "Connection to <ip> failed"
        );
    }

    #[test]
    fn replaces_whole_ipv6_addresses() {
        // Each form must be consumed entirely — a partial match leaves the tail to be rewritten as
        // <int>s, which makes two different addresses normalize differently.
        for address in [
            "2001:db8:85a3:0:0:8a2e:370:7334",
            "2001:db8::8a2e:370:7334",
            "2001:db8::8a2e:370",
            "2001:db8::1",
            "2001:db8::",
            "::1",
            "::",
            "fe80::1",
            "fe80::1%eth0",
            "::ffff:192.168.1.1",
        ] {
            assert_eq!(
                parameterize(&format!("bound to {address} ok")),
                "bound to <ip> ok",
                "incomplete match for {address}"
            );
        }
    }

    #[test]
    fn two_different_ipv6_addresses_normalize_alike() {
        assert_eq!(
            parameterize("peer 2001:db8::8a2e:370:7334 gone"),
            parameterize("peer 2001:db8::1428:57ab:1234 gone")
        );
    }

    #[test]
    fn replaces_uuids_and_hashes() {
        assert_eq!(
            parameterize("user 8ba1f9ce-1d2e-4c3b-9f8a-1b2c3d4e5f60 not found"),
            "user <uuid> not found"
        );
        assert_eq!(
            parameterize("blob da39a3ee5e6b4b0d3255bfef95601890afd80709 missing"),
            "blob <sha1> missing"
        );
        assert_eq!(
            parameterize("etag 9e107d9d372bb6826bd81d3542a419d6 stale"),
            "etag <md5> stale"
        );
    }

    #[test]
    fn replaces_numbers() {
        assert_eq!(parameterize("retried 5 times"), "retried <int> times");
        assert_eq!(parameterize("took 1.25 units"), "took <float> units");
        assert_eq!(parameterize("offset -42 invalid"), "offset <int> invalid");
        assert_eq!(
            parameterize("addr 0xDEADBEEF unmapped"),
            "addr <hex> unmapped"
        );
    }

    #[test]
    fn replaces_emails_and_urls() {
        assert_eq!(
            parameterize("no account for a.b+c@example.com"),
            "no account for <email>"
        );
        assert_eq!(
            parameterize("GET https://api.example.com/v1/users?id=3 failed"),
            "GET <url> failed"
        );
    }

    #[test]
    fn replaces_dates_and_durations() {
        assert_eq!(
            parameterize("expired at 2026-07-29T10:11:12Z"),
            "expired at <date>"
        );
        assert_eq!(
            parameterize("deadline 2026-07-29 passed"),
            "deadline <date> passed"
        );
        assert_eq!(
            parameterize("timed out after 250ms"),
            "timed out after <duration>"
        );
        assert_eq!(
            parameterize("timed out after 30s"),
            "timed out after <duration>"
        );
        assert_eq!(
            parameterize("timed out after 1.5s"),
            "timed out after <duration>"
        );
    }

    #[test]
    fn only_replaces_quoted_values_after_an_equals_sign() {
        assert_eq!(
            parameterize(r#"field="secret" bad"#),
            "field=<quoted_str> bad"
        );
        // A quoted string that is not a value is left alone.
        assert_eq!(
            parameterize(r#"the "widget" broke"#),
            r#"the "widget" broke"#
        );
    }

    #[test]
    fn only_replaces_bools_after_an_equals_sign() {
        assert_eq!(parameterize("enabled=true failed"), "enabled=<bool> failed");
        assert_eq!(parameterize("that is true"), "that is true");
    }

    #[test]
    fn leaves_a_message_with_no_variables_untouched() {
        assert_eq!(
            parameterize("connection reset by peer"),
            "connection reset by peer"
        );
    }

    #[test]
    fn messages_differing_only_in_variables_normalize_alike() {
        let a = normalize_message_for_grouping("Timeout talking to 10.0.0.5 after 30s (attempt 3)");
        let b = normalize_message_for_grouping("Timeout talking to 10.0.0.9 after 45s (attempt 7)");

        assert_eq!(a, b);
        assert!(a.contains("<ip>"), "unexpected normalization: {a}");
    }

    #[test]
    fn keeps_the_first_two_non_empty_lines_and_marks_the_truncation() {
        let normalized = normalize_message_for_grouping("first\n\nsecond\nthird\nfourth");
        assert_eq!(normalized, "first\nsecond...");
    }

    #[test]
    fn a_single_line_message_is_not_marked_as_truncated() {
        assert_eq!(
            normalize_message_for_grouping("just one line"),
            "just one line"
        );
    }

    #[test]
    fn a_trailing_newline_counts_as_truncation() {
        // The trimmed form differs from the input, so upstream appends the marker.
        assert_eq!(normalize_message_for_grouping("one line\n"), "one line...");
    }

    #[test]
    fn handles_an_empty_message() {
        assert_eq!(normalize_message_for_grouping(""), "");
    }
}
