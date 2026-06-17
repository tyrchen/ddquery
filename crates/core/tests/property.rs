//! Property tests: the parser must never panic, and `parse_or_unparsed` must
//! always yield a value for any input.

use ddquery_core::{MonitorQuery, explain, parse, parse_or_unparsed, summarize};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4096))]

    /// Arbitrary bytes must never panic and must round-trip through
    /// `parse_or_unparsed` → `explain`/`summarize`.
    #[test]
    fn prop_arbitrary_input_never_panics(input in ".*") {
        let _ = parse(&input);
        let parsed = parse_or_unparsed(&input);
        let _ = explain(&parsed);
        let _ = summarize(&parsed, &input);
    }

    /// Fuzz with query-flavored characters to exercise the grammar more.
    #[test]
    fn prop_query_flavored_input_never_panics(
        input in proptest::collection::vec(
            prop_oneof![
                Just("avg"), Just("sum"), Just("last_5m"), Just("("), Just(")"),
                Just("{"), Just("}"), Just(":"), Just("."), Just(","), Just("*"),
                Just(">"), Just(">="), Just("&&"), Just("!"), Just("123"),
                Just("anomalies"), Just("by"), Just("'agile'"), Just("metric.name"),
                Just("\""), Just("="),
            ],
            0..40,
        ).prop_map(|parts| parts.concat())
    ) {
        let parsed = parse_or_unparsed(&input);
        let _ = explain(&parsed);
        let _ = summarize(&parsed, &input);
    }

    /// Any successful parse must serialize to JSON and deserialize back to an
    /// equal AST.
    #[test]
    fn prop_successful_parse_roundtrips_json(
        agg in prop_oneof![Just("avg"), Just("sum"), Just("min"), Just("max")],
        n in 1u32..120,
        unit in prop_oneof![Just("m"), Just("h"), Just("d")],
        threshold in -1000i64..1000,
    ) {
        let query = format!("{agg}(last_{n}{unit}):{agg}:app.metric{{env:prod}} > {threshold}");
        if let Ok(ast) = parse(&query) {
            prop_assert!(matches!(ast, MonitorQuery::Metric(_)));
            let json = serde_json::to_string(&ast).unwrap();
            let back: MonitorQuery = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(ast, back);
        }
    }
}
