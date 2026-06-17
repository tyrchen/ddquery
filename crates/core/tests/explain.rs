//! Snapshot tests for the English `explain` output and the structured
//! `summarize` breakdown, one per dialect plus the hard cases.

use ddquery_core::{explain, parse, summarize};

/// The representative corpus, one entry per dialect and hard case.
const CORPUS: &[(&str, &str)] = &[
    (
        "simple_threshold",
        "avg(last_5m):avg:system.cpu.user{env:production, service:web} > 90",
    ),
    (
        "anomaly_flagship",
        "avg(last_1d):anomalies(sum:app.service.latency{platform:macos , country:us}.as_count(), \
         'agile', 5, direction='below', interval=300, seasonality='daily') >= 1",
    ),
    (
        "outlier_nested",
        "avg(last_5m):outliers(median_5(sum:app.service.requests{*} by {host}.as_count()), \
         'DBSCAN', 3) >= 1",
    ),
    (
        "arithmetic_ratio",
        "sum(last_5m):(sum:app.service.errors{*}.as_count() / \
         sum:app.service.requests{*}.as_count()) * 100 > 5",
    ),
    (
        "change_metric",
        "change(avg(last_5m),last_1h):avg:system.load.1{*} > 10",
    ),
    (
        "log_search",
        r#"logs("status:error @http.status_code:403").index("main").rollup("count").by("service").last("5m") > 100"#,
    ),
    (
        "service_check",
        r#""datadog.agent.up".over("env:prod").by("host").last(3).count_by_status()"#,
    ),
    (
        "slo_error_budget",
        r#"error_budget("slo-123").over("7d") > 0.5"#,
    ),
    ("composite", "123 && !456 || 789"),
];

#[test]
fn test_explain_snapshots() {
    let rendered: Vec<String> = CORPUS
        .iter()
        .map(|(name, query)| {
            let ast = parse(query).unwrap_or_else(|e| panic!("`{query}` failed: {e}"));
            format!("[{name}]\n{query}\n→ {}", explain(&ast))
        })
        .collect();
    insta::assert_snapshot!(rendered.join("\n\n"));
}

#[test]
fn test_summarize_snapshots() {
    let summaries: Vec<_> = CORPUS
        .iter()
        .map(|(name, query)| {
            let ast = parse(query).unwrap_or_else(|e| panic!("`{query}` failed: {e}"));
            (name.to_string(), summarize(&ast, query))
        })
        .collect();
    insta::assert_json_snapshot!(summaries);
}

#[test]
fn test_unparsed_explains_gracefully() {
    let ast = ddquery_core::parse_or_unparsed("totally unknown syntax");
    let text = explain(&ast);
    assert!(text.contains("could not be parsed"));
    let summary = summarize(&ast, "totally unknown syntax");
    assert_eq!(summary.headline, "Raw Datadog query");
    assert_eq!(summary.raw, "totally unknown syntax");
}
