//! The typed monitor-query AST.
//!
//! Every public type here derives `Serialize`/`Deserialize` (camelCase) and
//! `PartialEq`, so a parsed query round-trips cleanly to JSON for a front-end
//! and compares structurally in tests. Dialect-tagged enums carry a `type`
//! discriminant so the JSON is self-describing.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::ParseError;

/// A parsed Datadog monitor query, tagged by dialect.
///
/// The [`MonitorQuery::Unparsed`] variant is the graceful-degradation path: a
/// query the grammar does not yet model is preserved verbatim rather than
/// dropped, so a consumer can always render *something*.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum MonitorQuery {
    /// `time-agg(window): expr op threshold` — the primary dialect.
    Metric(MetricQuery),
    /// `logs/events/rum/error-tracking` search-source query.
    Search(SearchQuery),
    /// `"check".over(...).by(...).last(n).count_by_status()`.
    ServiceCheck(CheckQuery),
    /// `error_budget("<id>").over("7d") op n`.
    Slo(SloQuery),
    /// Boolean tree of monitor-id references.
    Composite(CompositeExpr),
    /// A query that could not be parsed; preserved verbatim with a reason.
    #[serde(rename_all = "camelCase")]
    Unparsed {
        /// The verbatim query string.
        raw: String,
        /// One-line reason the query did not parse.
        reason: String,
        /// Byte offset into `raw` where parsing failed.
        offset: usize,
    },
}

impl MonitorQuery {
    /// Build an [`MonitorQuery::Unparsed`] from a raw query and a [`ParseError`].
    #[must_use]
    pub fn unparsed(raw: impl Into<String>, err: &ParseError) -> Self {
        Self::Unparsed {
            raw: raw.into(),
            reason: err.reason().to_string(),
            offset: err.offset(),
        }
    }
}

/// A metric monitor query: `time_agg(window): expr op threshold`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricQuery {
    /// Evaluation time aggregation (`avg`, `sum`, `min`, `max`, `count`).
    pub time_aggregation: TimeAgg,
    /// The evaluation window (`last_5m`, `last_1d`, …).
    pub window: Window,
    /// The recursive metric expression being evaluated.
    pub expr: MetricExpr,
    /// The alerting condition (operator + thresholds).
    pub condition: Condition,
}

/// The recursive metric expression — the tree a visualization renders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum MetricExpr {
    /// A scoped metric: `space_agg:metric.name{filter} by {tags}.modifiers`.
    Series(Series),
    /// An evaluation function: `anomalies` / `outliers` / `forecast`.
    #[serde(rename_all = "camelCase")]
    Function {
        /// Which evaluation function.
        name: FuncKind,
        /// The inner expression the function evaluates.
        arg: Box<MetricExpr>,
        /// Positional and keyword parameters.
        params: Vec<FuncParam>,
    },
    /// A transform such as `rollup` / `clamp_min` / `median_N` / `abs` /
    /// `moving_rollup`.
    #[serde(rename_all = "camelCase")]
    Transform {
        /// Transform name (kept verbatim, e.g. `median_5`).
        name: String,
        /// The inner expression being transformed.
        arg: Box<MetricExpr>,
        /// Arguments to the transform. Most are numeric (e.g. `rollup(60)`), but
        /// some carry strings — e.g. `moving_rollup(expr, 60, 'avg')`.
        args: Vec<ParamValue>,
    },
    /// A spatial combiner over several sub-expressions, e.g.
    /// `max(avg:a{*}, avg:b{*})`. `name` is the aggregator token
    /// (`max`/`min`/`sum`/`avg`/`count`).
    #[serde(rename_all = "camelCase")]
    Combine {
        /// The aggregator token combining the operands.
        name: String,
        /// The combined sub-expressions (two or more in practice).
        args: Vec<MetricExpr>,
    },
    /// Arithmetic: `a + b`, `a / b * 100`, etc.
    #[serde(rename_all = "camelCase")]
    Arith {
        /// The operator.
        op: ArithOp,
        /// Left operand.
        lhs: Box<MetricExpr>,
        /// Right operand.
        rhs: Box<MetricExpr>,
    },
    /// `change()` / `pct_change()` over a window shift.
    #[serde(rename_all = "camelCase")]
    Change {
        /// Whether this is an absolute or percentage change.
        kind: ChangeKind,
        /// The inner time aggregation applied before the shift.
        inner_agg: TimeAgg,
        /// The window shift to compare against.
        shift: Window,
        /// The inner expression.
        arg: Box<MetricExpr>,
    },
    /// A literal scalar operand (for arithmetic).
    Scalar(Scalar),
}

/// A scoped metric series.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Series {
    /// Spatial aggregation across reporting sources.
    pub space_aggregation: SpaceAgg,
    /// The metric name, e.g. `system.cpu.user`.
    pub metric: String,
    /// Scope filters, e.g. `env:production`, `!result:ok`.
    pub filter: Vec<Filter>,
    /// `by {tag}` grouping tags.
    pub group_by: Vec<String>,
    /// Postfix modifiers, e.g. `as_count()`, `fill(zero)`.
    pub modifiers: Vec<Modifier>,
}

/// A scope filter inside `{ … }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Filter {
    /// `key:value`, optionally negated with a leading `!`.
    #[serde(rename_all = "camelCase")]
    Tag {
        /// Whether the filter is negated (`!key:value`).
        negated: bool,
        /// The tag key.
        key: String,
        /// The tag value.
        value: String,
    },
    /// The `*` match-all filter.
    All,
    /// A bare tag glob such as `service:web-*`.
    Glob(String),
}

/// A postfix series modifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Modifier {
    /// Modifier name, e.g. `as_count`, `fill`, `rollup`.
    pub name: String,
    /// Modifier arguments kept verbatim, e.g. `["zero"]` for `fill(zero)`.
    pub args: Vec<String>,
}

/// The alerting condition: comparison operator plus thresholds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Condition {
    /// Comparison operator.
    pub operator: CmpOp,
    /// The critical threshold.
    pub critical: Scalar,
    /// Optional critical-recovery threshold.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub critical_recovery: Option<Scalar>,
    /// Optional warning threshold.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<Scalar>,
    /// Optional warning-recovery threshold.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning_recovery: Option<Scalar>,
}

/// A log/event/rum/error-tracking search-source query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchQuery {
    /// The search source.
    pub source: SearchSource,
    /// The verbatim inner search string (Datadog's search mini-language is
    /// intentionally **not** parsed — stored as-is).
    pub raw_search: String,
    /// Optional `.index("…")`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<String>,
    /// `.rollup` method (e.g. `count`, `avg`).
    pub rollup_method: String,
    /// Optional `.rollup` interval / second argument.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rollup_arg: Option<String>,
    /// `.by("…")` grouping tags.
    pub group_by: Vec<String>,
    /// The `.last("…")` evaluation window argument.
    pub last: String,
    /// The alerting condition.
    pub condition: Condition,
}

/// The source of a [`SearchQuery`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SearchSource {
    /// `logs(...)`.
    Logs,
    /// `events(...)`.
    Events,
    /// `rum(...)`.
    Rum,
    /// `error-tracking(...)`.
    ErrorTracking,
}

/// A service-check query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckQuery {
    /// The check name.
    pub check: String,
    /// `.over(...)` scope tags.
    pub over: Vec<String>,
    /// `.by(...)` grouping tags.
    pub by: Vec<String>,
    /// `.last(n)` count of recent check runs.
    pub last: u32,
}

/// An SLO error-budget query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SloQuery {
    /// The SLO identifier.
    pub id: String,
    /// The `.over("…")` time window argument.
    pub over: String,
    /// The alerting condition.
    pub condition: Condition,
}

/// A boolean tree over monitor-id references (composite monitors).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CompositeExpr {
    /// A reference to another monitor by id.
    Ref(MonitorRef),
    /// Logical negation of a sub-expression.
    Not(Box<CompositeExpr>),
    /// A binary boolean combination.
    #[serde(rename_all = "camelCase")]
    Binary {
        /// `&&` or `||`.
        op: BoolOp,
        /// Left operand.
        lhs: Box<CompositeExpr>,
        /// Right operand.
        rhs: Box<CompositeExpr>,
    },
}

/// A reference to another monitor by its numeric id (kept as a string to
/// preserve the verbatim token).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorRef {
    /// The referenced monitor id.
    pub id: String,
}

/// A normalized evaluation window.
///
/// `last_5m` normalizes to `{ raw: "last_5m", seconds: 300, display: "last 5
/// minutes" }`. Used for sorting, comparison, and human display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Window {
    /// The verbatim window token.
    pub raw: String,
    /// The window length in seconds.
    pub seconds: u64,
    /// A human-readable rendering, e.g. `last 5 minutes`.
    pub display: String,
}

impl Window {
    /// Parse and normalize a window token such as `last_5m` or `5m`.
    ///
    /// Recognized prefixes are `last_` (the default) and `current_` (the
    /// in-progress period, e.g. `current_1d`). Recognized units are `s`, `m`,
    /// `h`, `d`, `w`, and `mo` (month, normalized to 30 days). Returns `None`
    /// if the token is not a recognized `<prefix><number><unit>` form.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        // `current_<n><unit>` describes the in-progress period; for length
        // purposes it normalizes identically to `last_<n><unit>`. We keep the
        // verbatim `raw` so the distinction survives for display.
        let (body, prefix_word) = if let Some(rest) = raw.strip_prefix("last_") {
            (rest, "last")
        } else if let Some(rest) = raw.strip_prefix("current_") {
            (rest, "current")
        } else {
            (raw, "last")
        };
        let (digits, unit) = body.split_at(body.find(|c: char| !c.is_ascii_digit())?);
        if digits.is_empty() {
            return None;
        }
        let count: u64 = digits.parse().ok()?;
        let (secs_per, unit_name) = match unit {
            "s" => (1, "second"),
            "m" => (60, "minute"),
            "h" => (3600, "hour"),
            "d" => (86_400, "day"),
            "w" => (604_800, "week"),
            "mo" => (2_592_000, "month"), // 30 days, matching Datadog
            _ => return None,
        };
        let seconds = count.checked_mul(secs_per)?;
        let plural = if count == 1 { "" } else { "s" };
        Some(Self {
            raw: raw.to_string(),
            seconds,
            display: format!("{prefix_word} {count} {unit_name}{plural}"),
        })
    }
}

/// A numeric scalar literal (threshold or arithmetic operand).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Scalar {
    /// The numeric value.
    pub value: f64,
}

impl Scalar {
    /// Construct a scalar from a value.
    #[must_use]
    pub fn new(value: f64) -> Self {
        Self { value }
    }
}

impl fmt::Display for Scalar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.value.fract() == 0.0 && self.value.is_finite() {
            write!(f, "{}", self.value as i64)
        } else {
            write!(f, "{}", self.value)
        }
    }
}

/// A parameter to an evaluation function.
///
/// A `None` `key` is a positional parameter (`'agile'`, `5`); a `Some` `key`
/// is a keyword parameter (`direction='below'`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FuncParam {
    /// The keyword name, if this is a keyword parameter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The parameter value.
    pub value: ParamValue,
}

impl FuncParam {
    /// A positional parameter.
    #[must_use]
    pub fn positional(value: ParamValue) -> Self {
        Self { key: None, value }
    }

    /// A keyword parameter.
    #[must_use]
    pub fn keyword(key: impl Into<String>, value: ParamValue) -> Self {
        Self {
            key: Some(key.into()),
            value,
        }
    }
}

/// The value of a [`FuncParam`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ParamValue {
    /// A boolean literal.
    Bool(bool),
    /// A numeric literal.
    Number(f64),
    /// A string literal (quotes stripped).
    Str(String),
}

impl fmt::Display for ParamValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParamValue::Bool(b) => write!(f, "{b}"),
            ParamValue::Number(n) => write!(f, "{n}"),
            ParamValue::Str(s) => write!(f, "{s}"),
        }
    }
}

macro_rules! str_enum {
    (
        $(#[$meta:meta])*
        $name:ident { $( $variant:ident => $text:literal ),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub enum $name {
            $(
                #[doc = concat!("`", $text, "`")]
                $variant,
            )+
        }

        impl $name {
            /// Parse from the source token, returning `None` if unrecognized.
            #[must_use]
            pub fn from_token(s: &str) -> Option<Self> {
                match s {
                    $( $text => Some(Self::$variant), )+
                    _ => None,
                }
            }

            /// The canonical source token.
            #[must_use]
            pub fn as_token(self) -> &'static str {
                match self {
                    $( Self::$variant => $text, )+
                }
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_token())
            }
        }
    };
}

str_enum! {
    /// Evaluation-time aggregation.
    TimeAgg {
        Avg => "avg",
        Sum => "sum",
        Min => "min",
        Max => "max",
        Count => "count",
        Percentile => "percentile",
    }
}

/// Spatial aggregation across reporting sources.
///
/// Besides the named aggregators, Datadog allows a percentile selector such as
/// `p95:` / `p99.9:`, which we keep as [`SpaceAgg::Percentile`] carrying the
/// percentile rank verbatim (e.g. `"95"`, `"99.9"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SpaceAgg {
    /// `avg`
    Avg,
    /// `sum`
    Sum,
    /// `min`
    Min,
    /// `max`
    Max,
    /// `count`
    Count,
    /// A percentile selector, e.g. `p95` → `Percentile("95")`. The rank is kept
    /// as text so fractional percentiles (`p99.9`) round-trip losslessly.
    Percentile(String),
}

impl SpaceAgg {
    /// Parse from the source token, returning `None` if unrecognized.
    ///
    /// Recognizes the named aggregators and percentile tokens `p<rank>`, where
    /// `<rank>` is a number 0–100 (optionally fractional), e.g. `p95`, `p99.9`.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "avg" => Some(Self::Avg),
            "sum" => Some(Self::Sum),
            "min" => Some(Self::Min),
            "max" => Some(Self::Max),
            "count" => Some(Self::Count),
            _ => {
                let rank = s.strip_prefix('p')?;
                let value: f64 = rank.parse().ok()?;
                (0.0..=100.0)
                    .contains(&value)
                    .then(|| Self::Percentile(rank.to_string()))
            }
        }
    }
}

impl ::std::fmt::Display for SpaceAgg {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        match self {
            Self::Avg => f.write_str("avg"),
            Self::Sum => f.write_str("sum"),
            Self::Min => f.write_str("min"),
            Self::Max => f.write_str("max"),
            Self::Count => f.write_str("count"),
            Self::Percentile(rank) => write!(f, "p{rank}"),
        }
    }
}

str_enum! {
    /// An evaluation function.
    FuncKind {
        Anomalies => "anomalies",
        Outliers => "outliers",
        Forecast => "forecast",
    }
}

str_enum! {
    /// An arithmetic operator.
    ArithOp {
        Add => "+",
        Sub => "-",
        Mul => "*",
        Div => "/",
    }
}

str_enum! {
    /// A change function kind.
    ChangeKind {
        Change => "change",
        PctChange => "pct_change",
    }
}

str_enum! {
    /// A boolean operator in a composite query.
    BoolOp {
        And => "&&",
        Or => "||",
    }
}

str_enum! {
    /// A comparison operator in a condition.
    CmpOp {
        Ge => ">=",
        Le => "<=",
        Eq => "==",
        Ne => "!=",
        Gt => ">",
        Lt => "<",
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("last_5m", 300, "last 5 minutes")]
    #[case("last_1m", 60, "last 1 minute")]
    #[case("last_1d", 86_400, "last 1 day")]
    #[case("last_1w", 604_800, "last 1 week")]
    #[case("last_30s", 30, "last 30 seconds")]
    #[case("4h", 14_400, "last 4 hours")]
    fn test_should_normalize_window(
        #[case] raw: &str,
        #[case] seconds: u64,
        #[case] display: &str,
    ) {
        let window = Window::parse(raw).expect("valid window");
        assert_eq!(window.seconds, seconds);
        assert_eq!(window.display, display);
        assert_eq!(window.raw, raw);
    }

    #[rstest]
    #[case("")]
    #[case("last_")]
    #[case("5x")]
    #[case("m")]
    #[case("last_5mm")]
    #[case("abc")]
    fn test_should_reject_invalid_window(#[case] raw: &str) {
        assert!(Window::parse(raw).is_none());
    }

    #[test]
    fn test_should_reject_overflowing_window() {
        assert!(Window::parse("18446744073709551615w").is_none());
    }

    #[test]
    fn test_should_render_scalar_without_trailing_zero() {
        assert_eq!(Scalar::new(90.0).to_string(), "90");
        assert_eq!(Scalar::new(0.5).to_string(), "0.5");
        assert_eq!(Scalar::new(-3.0).to_string(), "-3");
    }

    #[test]
    fn test_should_roundtrip_through_json() {
        let agg = TimeAgg::Avg;
        let json = serde_json::to_string(&agg).expect("serialize");
        assert_eq!(json, "\"avg\"");
        let back: TimeAgg = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, agg);
    }
}
