//! Deterministic AST → English and AST → structured-summary rendering.
//!
//! Both [`explain`] and [`summarize`] are pure functions over the AST. There
//! is no LLM and no I/O, so the output is stable and snapshot-testable. Richer
//! narration can layer on top later, but this base layer is the contract.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::ast::{
    ArithOp, ChangeKind, CheckQuery, CmpOp, CompositeExpr, Condition, Filter, FuncKind, FuncParam,
    MetricExpr, MetricQuery, MonitorQuery, ParamValue, SearchQuery, SearchSource, Series, SloQuery,
    TimeAgg,
};

/// A render-ready breakdown of a query for a front-end (labelled chips/rows).
///
/// Every field is optional except `raw` and `headline`, so a consumer can show
/// just the chips that apply. `raw` is always the verbatim query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuerySummary {
    /// A short headline, e.g. `Anomaly detection on view time`.
    pub headline: String,
    /// The primary metric name, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metric: Option<String>,
    /// Scope filters as key/value chips.
    pub scope: Vec<KeyVal>,
    /// `by {…}` grouping tags.
    pub group_by: Vec<String>,
    /// The evaluation window, humanized.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    /// The evaluation method (anomaly/outlier/forecast/change phrasing).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluation: Option<String>,
    /// The alerting condition, e.g. `≥ 1`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    /// The verbatim query string. Always present.
    pub raw: String,
}

/// A scope key/value pair (a filter chip).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyVal {
    /// The tag key.
    pub key: String,
    /// The tag value.
    pub value: String,
    /// Whether the filter is negated (`!key:value`).
    ///
    /// `default` pairs with `skip_serializing_if`: the field is omitted from JSON
    /// when `false`, so deserialization must also tolerate its absence — otherwise
    /// a serialize→deserialize round-trip of a non-negated filter fails with
    /// `missing field negated`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub negated: bool,
}

/// Produce a one-paragraph, plain-language summary of a parsed query.
///
/// # Examples
///
/// ```
/// use ddquery_core::{explain, parse};
///
/// let q = parse("avg(last_5m):avg:system.cpu.user{env:prod} > 90").unwrap();
/// let text = explain(&q);
/// assert!(text.contains("system.cpu.user"));
/// ```
#[must_use]
pub fn explain(query: &MonitorQuery) -> String {
    match query {
        MonitorQuery::Metric(m) => explain_metric(m),
        MonitorQuery::Search(s) => explain_search(s),
        MonitorQuery::ServiceCheck(c) => explain_check(c),
        MonitorQuery::Slo(s) => explain_slo(s),
        MonitorQuery::Composite(c) => {
            format!(
                "Alerts when this boolean condition over other monitors holds: {}.",
                render_composite(c)
            )
        }
        MonitorQuery::Unparsed { reason, .. } => {
            format!("Raw Datadog query (could not be parsed: {reason}).")
        }
    }
}

/// Produce a structured, render-ready summary of a parsed query.
///
/// The `raw` argument is the original query string, preserved verbatim in the
/// summary so the front-end can offer a "show raw" toggle.
#[must_use]
pub fn summarize(query: &MonitorQuery, raw: &str) -> QuerySummary {
    let mut summary = QuerySummary {
        headline: String::new(),
        metric: None,
        scope: Vec::new(),
        group_by: Vec::new(),
        window: None,
        evaluation: None,
        condition: None,
        raw: raw.to_string(),
    };
    match query {
        MonitorQuery::Metric(m) => summarize_metric(m, &mut summary),
        MonitorQuery::Search(s) => summarize_search(s, &mut summary),
        MonitorQuery::ServiceCheck(c) => {
            summary.headline = format!("Service check `{}`", c.check);
            summary.group_by = c.by.clone();
            summary.condition = Some(format!("last {} runs by status", c.last));
        }
        MonitorQuery::Slo(s) => {
            summary.headline = format!("SLO error budget `{}`", s.id);
            summary.window = Some(s.over.clone());
            summary.condition = Some(render_condition(&s.condition));
        }
        MonitorQuery::Composite(c) => {
            summary.headline = "Composite monitor".to_string();
            summary.condition = Some(render_composite(c));
        }
        MonitorQuery::Unparsed { .. } => {
            summary.headline = "Raw Datadog query".to_string();
        }
    }
    summary
}

// ----- metric --------------------------------------------------------------

fn explain_metric(m: &MetricQuery) -> String {
    let mut out = String::new();
    let _ = write!(
        out,
        "Alert if the {} over the {} of {} is {}.",
        time_agg_word(m.time_aggregation),
        m.window.display.trim_start_matches("last "),
        describe_expr(&m.expr),
        render_condition(&m.condition),
    );
    out
}

fn time_agg_word(agg: TimeAgg) -> &'static str {
    match agg {
        TimeAgg::Avg => "average",
        TimeAgg::Sum => "sum",
        TimeAgg::Min => "minimum",
        TimeAgg::Max => "maximum",
        TimeAgg::Count => "count",
        TimeAgg::Percentile => "percentile",
    }
}

fn summarize_metric(m: &MetricQuery, summary: &mut QuerySummary) {
    if let Some(series) = first_series(&m.expr) {
        summary.metric = Some(series.metric.clone());
        summary.scope = series.filter.iter().filter_map(filter_to_keyval).collect();
        summary.group_by = series.group_by.clone();
    }
    summary.window = Some(m.window.display.clone());
    summary.evaluation = evaluation_phrase(&m.expr);
    summary.condition = Some(render_condition(&m.condition));
    summary.headline = metric_headline(&m.expr, summary.metric.as_deref());
}

fn metric_headline(expr: &MetricExpr, metric: Option<&str>) -> String {
    let subject = metric.unwrap_or("metric");
    match top_function(expr) {
        Some(FuncKind::Anomalies) => format!("Anomaly detection on {subject}"),
        Some(FuncKind::Outliers) => format!("Outlier detection on {subject}"),
        Some(FuncKind::Forecast) => format!("Forecast alert on {subject}"),
        None => match expr {
            MetricExpr::Change { kind, .. } => format!("{} on {subject}", change_word(*kind, true)),
            _ => format!("Threshold alert on {subject}"),
        },
    }
}

/// A best-effort English description of the expression tree.
fn describe_expr(expr: &MetricExpr) -> String {
    match expr {
        MetricExpr::Series(s) => describe_series(s),
        MetricExpr::Function { name, arg, params } => {
            let inner = describe_expr(arg);
            format!("{} ({inner})", function_phrase(*name, params))
        }
        MetricExpr::Transform { name, arg, .. } => {
            format!("{} of {}", humanize_transform(name), describe_expr(arg))
        }
        MetricExpr::Combine { name, args } => {
            let parts: Vec<String> = args.iter().map(describe_expr).collect();
            format!("{name} of ({})", parts.join(", "))
        }
        MetricExpr::Arith { op, lhs, rhs } => {
            format!(
                "({} {} {})",
                describe_expr(lhs),
                arith_word(*op),
                describe_expr(rhs)
            )
        }
        MetricExpr::Change {
            kind, shift, arg, ..
        } => format!(
            "{} over {} of {}",
            change_word(*kind, false),
            shift.display,
            describe_expr(arg)
        ),
        MetricExpr::Scalar(s) => s.to_string(),
    }
}

fn describe_series(s: &Series) -> String {
    let mut text = format!("{} of `{}`", s.space_aggregation, s.metric);
    let scope: Vec<String> = s
        .filter
        .iter()
        .filter_map(filter_to_keyval)
        .map(|kv| {
            if kv.negated {
                format!("not {}:{}", kv.key, kv.value)
            } else {
                format!("{}:{}", kv.key, kv.value)
            }
        })
        .collect();
    if !scope.is_empty() {
        let _ = write!(text, " ({})", scope.join(", "));
    }
    if !s.group_by.is_empty() {
        let _ = write!(text, " by {}", s.group_by.join(", "));
    }
    text
}

fn function_phrase(name: FuncKind, params: &[FuncParam]) -> String {
    match name {
        FuncKind::Anomalies => {
            let algo = positional_str(params, 0).unwrap_or("basic");
            let bound = param_count(params, 1);
            let direction = keyword_str(params, "direction").unwrap_or("both");
            let seasonality = keyword_str(params, "seasonality");
            let mut phrase = format!("anomaly detection (algorithm {algo}");
            if let Some(n) = bound {
                let _ = write!(phrase, ", ~{}σ", trim_num(n));
            }
            let _ = write!(phrase, ", direction {direction}");
            if let Some(season) = seasonality {
                let _ = write!(phrase, ", {season} seasonality");
            }
            phrase.push(')');
            phrase
        }
        FuncKind::Outliers => {
            let algo = positional_str(params, 0).unwrap_or("DBSCAN");
            let tolerance = param_count(params, 1);
            let mut phrase = format!("outlier detection (algorithm {algo}");
            if let Some(n) = tolerance {
                let _ = write!(phrase, ", tolerance {}", trim_num(n));
            }
            phrase.push(')');
            phrase
        }
        FuncKind::Forecast => {
            let algo = positional_str(params, 0).unwrap_or("linear");
            format!("forecast (algorithm {algo})")
        }
    }
}

fn evaluation_phrase(expr: &MetricExpr) -> Option<String> {
    match expr {
        MetricExpr::Function { name, params, .. } => Some(function_phrase(*name, params)),
        MetricExpr::Change { kind, shift, .. } => Some(format!(
            "{} over {}",
            change_word(*kind, false),
            shift.display
        )),
        MetricExpr::Arith { lhs, rhs, .. } => {
            evaluation_phrase(lhs).or_else(|| evaluation_phrase(rhs))
        }
        MetricExpr::Transform { arg, .. } => evaluation_phrase(arg),
        _ => None,
    }
}

// ----- search / check / slo / composite ------------------------------------

fn explain_search(s: &SearchQuery) -> String {
    let by = if s.group_by.is_empty() {
        String::new()
    } else {
        format!(", grouped by {}", s.group_by.join(", "))
    };
    format!(
        "Over {}, count {} matching `{}`{by} (rollup {}), alert {}.",
        s.last,
        source_noun(s.source),
        s.raw_search,
        s.rollup_method,
        render_condition(&s.condition),
    )
}

fn summarize_search(s: &SearchQuery, summary: &mut QuerySummary) {
    summary.headline = format!("{} search", source_label(s.source));
    summary.metric = Some(s.raw_search.clone());
    summary.group_by = s.group_by.clone();
    summary.window = Some(s.last.clone());
    summary.evaluation = Some(format!("rollup {}", s.rollup_method));
    summary.condition = Some(render_condition(&s.condition));
}

fn explain_check(c: &CheckQuery) -> String {
    let by = if c.by.is_empty() {
        String::new()
    } else {
        format!(" grouped by {}", c.by.join(", "))
    };
    format!(
        "Evaluates the status of service check `{}` over its last {} runs{by}.",
        c.check, c.last
    )
}

fn explain_slo(s: &SloQuery) -> String {
    format!(
        "Alerts when the error budget for SLO `{}` over {} is {}.",
        s.id,
        s.over,
        render_condition(&s.condition)
    )
}

fn render_composite(c: &CompositeExpr) -> String {
    match c {
        CompositeExpr::Ref(r) => format!("#{}", r.id),
        CompositeExpr::Not(inner) => format!("!{}", render_composite(inner)),
        CompositeExpr::Binary { op, lhs, rhs } => {
            format!(
                "({} {} {})",
                render_composite(lhs),
                op,
                render_composite(rhs)
            )
        }
    }
}

// ----- shared helpers -------------------------------------------------------

fn render_condition(c: &Condition) -> String {
    format!("{} {}", cmp_symbol(c.operator), c.critical)
}

fn cmp_symbol(op: CmpOp) -> &'static str {
    match op {
        CmpOp::Ge => "≥",
        CmpOp::Le => "≤",
        CmpOp::Eq => "=",
        CmpOp::Ne => "≠",
        CmpOp::Gt => ">",
        CmpOp::Lt => "<",
    }
}

fn arith_word(op: ArithOp) -> &'static str {
    match op {
        ArithOp::Add => "+",
        ArithOp::Sub => "-",
        ArithOp::Mul => "×",
        ArithOp::Div => "÷",
    }
}

fn change_word(kind: ChangeKind, capitalized: bool) -> &'static str {
    match (kind, capitalized) {
        (ChangeKind::Change, true) => "Change",
        (ChangeKind::Change, false) => "change",
        (ChangeKind::PctChange, true) => "Percentage change",
        (ChangeKind::PctChange, false) => "percentage change",
    }
}

fn humanize_transform(name: &str) -> String {
    if let Some(n) = name.strip_prefix("median_") {
        return format!("{n}-point median");
    }
    if let Some(n) = name.strip_prefix("ewma_") {
        return format!("{n}-point EWMA");
    }
    match name {
        "rollup" => "rollup".to_string(),
        "clamp_min" => "lower-clamped value".to_string(),
        "clamp_max" => "upper-clamped value".to_string(),
        "abs" => "absolute value".to_string(),
        "count_nonzero" => "non-zero count".to_string(),
        other => other.replace('_', " "),
    }
}

fn source_noun(source: SearchSource) -> &'static str {
    match source {
        SearchSource::Logs => "log events",
        SearchSource::Events => "events",
        SearchSource::Rum => "RUM events",
        SearchSource::ErrorTracking => "tracked errors",
    }
}

fn source_label(source: SearchSource) -> &'static str {
    match source {
        SearchSource::Logs => "Log",
        SearchSource::Events => "Event",
        SearchSource::Rum => "RUM",
        SearchSource::ErrorTracking => "Error-tracking",
    }
}

fn filter_to_keyval(f: &Filter) -> Option<KeyVal> {
    match f {
        Filter::Tag {
            negated,
            key,
            value,
        } => Some(KeyVal {
            key: key.clone(),
            value: value.clone(),
            negated: *negated,
        }),
        Filter::All | Filter::Glob(_) => None,
    }
}

/// The first [`Series`] reachable in the expression tree, if any.
fn first_series(expr: &MetricExpr) -> Option<&Series> {
    match expr {
        MetricExpr::Series(s) => Some(s),
        MetricExpr::Function { arg, .. }
        | MetricExpr::Transform { arg, .. }
        | MetricExpr::Change { arg, .. } => first_series(arg),
        MetricExpr::Arith { lhs, rhs, .. } => first_series(lhs).or_else(|| first_series(rhs)),
        MetricExpr::Combine { args, .. } => args.iter().find_map(first_series),
        MetricExpr::Scalar(_) => None,
    }
}

/// The top-most evaluation function in the tree, if any.
fn top_function(expr: &MetricExpr) -> Option<FuncKind> {
    match expr {
        MetricExpr::Function { name, .. } => Some(*name),
        MetricExpr::Transform { arg, .. } | MetricExpr::Change { arg, .. } => top_function(arg),
        MetricExpr::Arith { lhs, rhs, .. } => top_function(lhs).or_else(|| top_function(rhs)),
        _ => None,
    }
}

fn positional_str(params: &[FuncParam], index: usize) -> Option<&str> {
    params
        .iter()
        .filter(|p| p.key.is_none())
        .nth(index)
        .and_then(|p| match &p.value {
            ParamValue::Str(s) => Some(s.as_str()),
            _ => None,
        })
}

fn param_count(params: &[FuncParam], index: usize) -> Option<f64> {
    params
        .iter()
        .filter(|p| p.key.is_none())
        .nth(index)
        .and_then(|p| match &p.value {
            ParamValue::Number(n) => Some(*n),
            _ => None,
        })
}

fn keyword_str<'a>(params: &'a [FuncParam], key: &str) -> Option<&'a str> {
    params
        .iter()
        .find(|p| p.key.as_deref() == Some(key))
        .map(|p| match &p.value {
            ParamValue::Str(s) => s.as_str(),
            ParamValue::Bool(true) => "true",
            ParamValue::Bool(false) => "false",
            ParamValue::Number(_) => "",
        })
        .filter(|s| !s.is_empty())
}

fn trim_num(n: f64) -> String {
    if n.fract() == 0.0 && n.is_finite() {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}
