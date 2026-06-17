![build](https://github.com/tyrchen/ddquery/workflows/build/badge.svg)

# ddquery

Parse and explain **Datadog monitor queries** in pure Rust.

A Datadog monitor query packs an aggregation window, a metric, a scope, a
grouping, transforms, an evaluation function, and a threshold into one dense
line:

```text
avg(last_1d):anomalies(sum:app.service.latency{platform:macos, country:us}.as_count(), 'agile', 5, direction='below') >= 1
```

`ddquery` turns that string into a typed, serializable AST and renders it back
as either a plain-language paragraph or a structured, render-ready breakdown:

```text
Alert if the average over the 1 day of anomaly detection
(algorithm agile, ~5σ, direction below) (sum of `app.service.latency`
(platform:macos, country:us)) is ≥ 1.
```

## Why a real parser

The query is a small but genuinely recursive language: an evaluation function
nests over a rollup over a space-aggregation over a scoped metric, with
arithmetic (`a / b * 100`), `clamp_min(clamp_max(...))`, and boolean composites
(`123 && !456`) making it a tree, not a flat record. Regex extraction loses
structure, mishandles nesting and quoting, and can't explain *why* a monitor
fires. A grammar-driven parser is the correct tool, and the language is small
and stable enough to own in-process — a hand-written recursive descent with a
small Pratt loop for arithmetic precedence, no parser-generator dependency.

## Features

- **Five dialects**: metric (`query`/`metric alert`), search
  (`logs`/`events`/`rum`/`error-tracking`), service check, SLO error-budget,
  and boolean composite.
- **Never panics on input** — every failure returns a `ParseError` with a byte
  offset and a one-line reason; fuzz-tested.
- **Graceful degradation** — `parse_or_unparsed` turns an unrecognized query
  into `MonitorQuery::Unparsed { raw, reason, offset }` so a consumer can always
  render the original verbatim, never dropping a record.
- **Bounded recursion** — pathologically nested input is rejected before the
  stack grows.
- **Pure & fast** — no I/O, no global state, linear in the input length.
- **Serde-ready** — the AST and summary serialize to camelCase JSON for a
  front-end.

## Usage

```rust
use ddquery_core::{explain, parse, summarize, MonitorQuery};

let query = "avg(last_5m):avg:system.cpu.user{env:production} > 90";
let ast = parse(query)?;

assert!(matches!(ast, MonitorQuery::Metric(_)));
println!("{}", explain(&ast));            // English paragraph
let summary = summarize(&ast, query);     // structured chips for a UI
# Ok::<(), ddquery_core::ParseError>(())
```

### Command line

```bash
# pretty output
cargo run -p ddquery-cli -- 'error_budget("slo-99").over("30d") < 0.99'

# machine-readable bundle (ast + summary + explanation)
echo 'avg(last_5m):avg:system.cpu.user{env:prod} > 90' | cargo run -p ddquery-cli -- --json
```

### Example

```bash
cargo run -p ddquery-core --example explain -- '<your query>'
```

## Crate layout

| Crate | What |
| --- | --- |
| `ddquery-core` | the library: AST, parser, `explain`/`summarize` |
| `ddquery-cli` | a thin command-line front-end (the `ddquery` binary) |

## License

This project is distributed under the terms of the MIT license.

See [LICENSE](LICENSE.md) for details.

Copyright 2025 Tyr Chen
