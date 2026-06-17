//! Parse and explain Datadog monitor queries.
//!
//! A Datadog monitor query packs an aggregation window, a metric, a scope, a
//! grouping, transforms, an evaluation function, and a threshold into one
//! dense line:
//!
//! ```text
//! avg(last_1d):anomalies(sum:app.service.latency{platform:macos, country:us}.as_count(), 'agile', 5, direction='below') >= 1
//! ```
//!
//! This crate turns that string into a typed, serializable AST
//! ([`MonitorQuery`]) and renders it back as either a plain-language paragraph
//! ([`explain`]) or a structured, render-ready breakdown ([`summarize`]).
//!
//! # Design
//!
//! - **Hand-written recursive descent + a small Pratt loop** for arithmetic precedence. No
//!   parser-generator dependency: the grammar is small and stable.
//! - **Never panics on input.** Every failure path returns a [`ParseError`] carrying a byte offset
//!   and a one-line reason.
//! - **Graceful degradation.** [`parse_or_unparsed`] never fails: a query the grammar does not yet
//!   model becomes [`MonitorQuery::Unparsed`] with the raw string preserved, so a consumer can
//!   always render *something*.
//! - **Bounded recursion.** Pathologically nested input is rejected before the stack can grow.
//! - **Pure.** No I/O, no global state; parsing is linear in the input length.
//!
//! # Dialects
//!
//! | Dialect | Shape | AST |
//! | --- | --- | --- |
//! | metric | `time-agg(window): expr op threshold` | [`MetricQuery`] |
//! | search | `logs("…").rollup(…).last(…) op n` | [`SearchQuery`] |
//! | service check | `"check".over(…).last(n).count_by_status()` | [`CheckQuery`] |
//! | slo | `error_budget("id").over("7d") op n` | [`SloQuery`] |
//! | composite | `123 && !456 \|\| 789` | [`CompositeExpr`] |
//!
//! # Examples
//!
//! ```
//! use ddquery_core::{explain, parse, MonitorQuery};
//!
//! let query = "avg(last_5m):avg:system.cpu.user{env:production} > 90";
//! let ast = parse(query).unwrap();
//! assert!(matches!(ast, MonitorQuery::Metric(_)));
//! println!("{}", explain(&ast));
//! ```

mod ast;
mod cursor;
mod error;
mod explain;
mod parser;

pub use ast::{
    ArithOp, BoolOp, ChangeKind, CheckQuery, CmpOp, CompositeExpr, Condition, Filter, FuncKind,
    FuncParam, MetricExpr, MetricQuery, Modifier, MonitorQuery, MonitorRef, ParamValue, Scalar,
    SearchQuery, SearchSource, Series, SloQuery, SpaceAgg, TimeAgg, Window,
};
pub use error::ParseError;
pub use explain::{KeyVal, QuerySummary, explain, summarize};
pub use parser::parse;

/// Parse a query, degrading to [`MonitorQuery::Unparsed`] on failure.
///
/// This is the entry point for offline batch processing where a single
/// unparsable query must never drop the surrounding record: the raw string,
/// failure reason, and byte offset are preserved so a consumer can still render
/// the original query verbatim.
///
/// # Examples
///
/// ```
/// use ddquery_core::{parse_or_unparsed, MonitorQuery};
///
/// let q = parse_or_unparsed("this is not a valid query !!!");
/// assert!(matches!(q, MonitorQuery::Unparsed { .. }));
/// ```
#[must_use]
pub fn parse_or_unparsed(query: &str) -> MonitorQuery {
    match parse(query) {
        Ok(parsed) => parsed,
        Err(err) => MonitorQuery::unparsed(query, &err),
    }
}
