//! Golden-AST and behavioral tests across every dialect, plus the hard cases
//! called out in the grammar notes.

use ddquery_core::{
    ArithOp, ChangeKind, CmpOp, CompositeExpr, Filter, FuncKind, MetricExpr, MonitorQuery,
    ParamValue, SearchSource, parse, parse_or_unparsed,
};
use rstest::rstest;

/// Extract the metric query or panic with a helpful message.
fn metric(query: &str) -> ddquery_core::MetricQuery {
    match parse(query).unwrap_or_else(|e| panic!("parse failed for `{query}`: {e}")) {
        MonitorQuery::Metric(m) => m,
        other => panic!("expected metric, got {other:?}"),
    }
}

#[test]
fn test_should_parse_simple_metric_query() {
    let m = metric("avg(last_5m):avg:system.cpu.user{env:production} > 90");
    assert_eq!(m.time_aggregation.as_token(), "avg");
    assert_eq!(m.window.seconds, 300);
    assert_eq!(m.condition.operator, CmpOp::Gt);
    assert_eq!(m.condition.critical.value, 90.0);

    let MetricExpr::Series(series) = &m.expr else {
        panic!("expected a series expression");
    };
    assert_eq!(series.metric, "system.cpu.user");
    assert_eq!(series.space_aggregation.as_token(), "avg");
    assert_eq!(
        series.filter,
        vec![Filter::Tag {
            negated: false,
            key: "env".into(),
            value: "production".into(),
        }]
    );
}

#[test]
fn test_should_parse_flagship_anomaly_query() {
    let q = "avg(last_1d):anomalies(sum:app.service.latency{platform:macos , \
             country:us}.as_count(), 'agile', 5, direction='below', interval=300, \
             alert_window='last_4h', count_default_zero='true', seasonality='daily', \
             timezone='America/Los_Angeles') >= 1";
    let m = metric(q);
    assert_eq!(m.condition.operator, CmpOp::Ge);

    let MetricExpr::Function { name, arg, params } = &m.expr else {
        panic!("expected a function expression");
    };
    assert_eq!(*name, FuncKind::Anomalies);

    // Positional params come first, then keywords.
    assert_eq!(params[0].key, None);
    assert_eq!(params[0].value, ParamValue::Str("agile".into()));
    assert_eq!(params[1].key, None);
    assert_eq!(params[1].value, ParamValue::Number(5.0));
    assert_eq!(params[2].key.as_deref(), Some("direction"));
    assert_eq!(params[2].value, ParamValue::Str("below".into()));

    let MetricExpr::Series(series) = arg.as_ref() else {
        panic!("anomaly arg should be a series");
    };
    assert_eq!(series.metric, "app.service.latency");
    assert_eq!(series.filter.len(), 2);
    assert_eq!(series.modifiers[0].name, "as_count");
}

#[rstest]
#[case("{platform:macos , country:us}")]
#[case("{platform:macos,country:us}")]
#[case("{platform:macos country:us}")]
fn test_should_treat_comma_and_space_filter_separators_equivalently(#[case] filter: &str) {
    let q = format!("avg(last_5m):sum:app.metric{filter} > 1");
    let m = metric(&q);
    let MetricExpr::Series(series) = &m.expr else {
        panic!("expected a series");
    };
    assert_eq!(series.filter.len(), 2);
}

#[test]
fn test_should_parse_negated_wildcard_and_glob_filters() {
    // `!result:ok` is a negated tag; `*` matches all; `service:web-*` is a tag
    // with a wildcard value; a colon-less `web-pool` is a bare glob.
    let m = metric("avg(last_5m):sum:app.metric{!result:ok, *, service:web-*, web-pool} > 1");
    let MetricExpr::Series(series) = &m.expr else {
        panic!("expected a series");
    };
    assert_eq!(
        series.filter,
        vec![
            Filter::Tag {
                negated: true,
                key: "result".into(),
                value: "ok".into(),
            },
            Filter::All,
            Filter::Tag {
                negated: false,
                key: "service".into(),
                value: "web-*".into(),
            },
            Filter::Glob("web-pool".into()),
        ]
    );
}

#[test]
fn test_should_parse_group_by_and_modifiers() {
    let m = metric(
        "sum(last_15m):sum:app.service.requests{*} by {host, route}.as_count().rollup(sum, 60) > 5",
    );
    let MetricExpr::Series(series) = &m.expr else {
        panic!("expected a series");
    };
    assert_eq!(series.group_by, vec!["host", "route"]);
    assert_eq!(series.modifiers.len(), 2);
    assert_eq!(series.modifiers[0].name, "as_count");
    assert_eq!(series.modifiers[1].name, "rollup");
    assert_eq!(series.modifiers[1].args, vec!["sum", "60"]);
}

#[test]
fn test_should_honor_arithmetic_precedence() {
    // a + b * c  ⇒  a + (b * c)
    let m = metric("sum(last_5m):sum:a.x{*} + sum:b.x{*} * sum:c.x{*} > 1");
    let MetricExpr::Arith { op, rhs, .. } = &m.expr else {
        panic!("expected arithmetic at the root");
    };
    assert_eq!(*op, ArithOp::Add);
    assert!(matches!(
        rhs.as_ref(),
        MetricExpr::Arith {
            op: ArithOp::Mul,
            ..
        }
    ));
}

#[test]
fn test_should_respect_explicit_parentheses() {
    let m = metric("sum(last_5m):(sum:a.x{*}.as_count() / sum:b.x{*}.as_count()) * 100 > 5");
    let MetricExpr::Arith { op, lhs, .. } = &m.expr else {
        panic!("expected arithmetic at the root");
    };
    assert_eq!(*op, ArithOp::Mul);
    assert!(matches!(
        lhs.as_ref(),
        MetricExpr::Arith {
            op: ArithOp::Div,
            ..
        }
    ));
}

#[test]
fn test_should_parse_nested_outlier_over_median_transform() {
    let m = metric(
        "avg(last_5m):outliers(median_5(sum:app.service.requests{*} by {host}.as_count()), \
         'DBSCAN', 3) >= 1",
    );
    let MetricExpr::Function { name, arg, params } = &m.expr else {
        panic!("expected a function");
    };
    assert_eq!(*name, FuncKind::Outliers);
    assert_eq!(params[0].value, ParamValue::Str("DBSCAN".into()));
    assert_eq!(params[1].value, ParamValue::Number(3.0));
    assert!(matches!(arg.as_ref(), MetricExpr::Transform { .. }));
}

#[test]
fn test_should_parse_change_metric() {
    let m = metric("change(avg(last_5m),last_1h):avg:system.load.1{*} > 10");
    assert_eq!(m.window.seconds, 300);
    let MetricExpr::Change {
        kind, shift, arg, ..
    } = &m.expr
    else {
        panic!("expected a change node");
    };
    assert_eq!(*kind, ChangeKind::Change);
    assert_eq!(shift.seconds, 3600);
    assert!(matches!(arg.as_ref(), MetricExpr::Series(_)));
}

#[test]
fn test_should_parse_log_search_query() {
    let q = r#"logs("status:error @http.status_code:403").index("main").rollup("count").by("service").last("5m") > 100"#;
    let MonitorQuery::Search(s) = parse(q).unwrap() else {
        panic!("expected a search query");
    };
    assert_eq!(s.source, SearchSource::Logs);
    assert_eq!(s.raw_search, "status:error @http.status_code:403");
    assert_eq!(s.index.as_deref(), Some("main"));
    assert_eq!(s.rollup_method, "count");
    assert_eq!(s.group_by, vec!["service"]);
    assert_eq!(s.last, "5m");
    assert_eq!(s.condition.operator, CmpOp::Gt);
}

#[test]
fn test_should_parse_error_tracking_source() {
    let q = r#"error-tracking("@issue.id:abc").rollup("count").last("1h") >= 5"#;
    let MonitorQuery::Search(s) = parse(q).unwrap() else {
        panic!("expected a search query");
    };
    assert_eq!(s.source, SearchSource::ErrorTracking);
    assert!(s.index.is_none());
}

#[test]
fn test_should_parse_service_check_query() {
    let q = r#""datadog.agent.up".over("env:prod").by("host").last(3).count_by_status()"#;
    let MonitorQuery::ServiceCheck(c) = parse(q).unwrap() else {
        panic!("expected a check query");
    };
    assert_eq!(c.check, "datadog.agent.up");
    assert_eq!(c.over, vec!["env:prod"]);
    assert_eq!(c.by, vec!["host"]);
    assert_eq!(c.last, 3);
}

#[test]
fn test_should_parse_slo_query() {
    let MonitorQuery::Slo(s) = parse(r#"error_budget("slo-123").over("7d") > 0.5"#).unwrap() else {
        panic!("expected an SLO query");
    };
    assert_eq!(s.id, "slo-123");
    assert_eq!(s.over, "7d");
    assert_eq!(s.condition.critical.value, 0.5);
}

#[test]
fn test_should_parse_composite_with_precedence() {
    // && binds tighter than || ⇒ (123 && !456) || 789
    let MonitorQuery::Composite(c) = parse("123 && !456 || 789").unwrap() else {
        panic!("expected a composite query");
    };
    let CompositeExpr::Binary { op, lhs, rhs } = &c else {
        panic!("expected a binary root");
    };
    assert_eq!(op.as_token(), "||");
    assert!(matches!(rhs.as_ref(), CompositeExpr::Ref(r) if r.id == "789"));
    assert!(matches!(
        lhs.as_ref(),
        CompositeExpr::Binary { op, .. } if op.as_token() == "&&"
    ));
}

#[test]
fn test_should_parse_parenthesized_composite() {
    let MonitorQuery::Composite(c) = parse("123 && (456 || 789)").unwrap() else {
        panic!("expected a composite query");
    };
    let CompositeExpr::Binary { op, rhs, .. } = &c else {
        panic!("expected a binary root");
    };
    assert_eq!(op.as_token(), "&&");
    assert!(matches!(
        rhs.as_ref(),
        CompositeExpr::Binary { op, .. } if op.as_token() == "||"
    ));
}

#[rstest]
#[case("")]
#[case("avg(last_5m)")]
#[case("avg(last_5m):")]
#[case("avg(last_5m):avg:metric{} >")]
#[case("notakeyword(last_5m):avg:x{*} > 1")]
#[case("avg(bogus):avg:x{*} > 1")]
#[case("logs(\"x\").last(\"5m\") > 1")]
fn test_should_return_error_not_panic(#[case] query: &str) {
    let err = parse(query).unwrap_err();
    assert!(!err.reason().is_empty());
}

#[test]
fn test_should_reject_unbalanced_parens_with_offset() {
    let err = parse("sum(last_5m):(sum:a.x{*} > 1").unwrap_err();
    assert!(err.reason().contains("expected `)`"), "{}", err.reason());
}

#[test]
fn test_should_degrade_unparsable_query_to_unparsed() {
    let q = "some brand new datadog syntax we don't model yet";
    let parsed = parse_or_unparsed(q);
    let MonitorQuery::Unparsed { raw, reason, .. } = parsed else {
        panic!("expected unparsed");
    };
    assert_eq!(raw, q);
    assert!(!reason.is_empty());
}

#[test]
fn test_should_reject_pathologically_nested_input() {
    let deep = format!(
        "sum(last_5m):{}sum:a.x{{*}}{} > 1",
        "(".repeat(500),
        ")".repeat(500)
    );
    // Must return an error, never overflow the stack.
    assert!(parse(&deep).is_err());
}

#[test]
fn test_should_serialize_ast_to_camel_case_json() {
    let ast = parse("avg(last_5m):avg:system.cpu.user{env:prod} by {host} > 90").unwrap();
    let json = serde_json::to_value(&ast).unwrap();
    let series = &json["metric"]["expr"]["series"];
    assert_eq!(series["spaceAggregation"], "avg");
    assert_eq!(series["groupBy"][0], "host");
    assert_eq!(json["metric"]["timeAggregation"], "avg");
}
