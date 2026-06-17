//! Parse a Datadog monitor query and print its AST, structured summary, and
//! plain-language explanation.
//!
//! ```bash
//! cargo run -p ddquery-core --example explain -- 'avg(last_5m):avg:system.cpu.user{env:prod} > 90'
//! ```
//!
//! With no argument, a built-in sample query is used.

use std::env;

use ddquery_core::{explain, parse_or_unparsed, summarize};

fn main() {
    let query = env::args().nth(1).unwrap_or_else(|| {
        "avg(last_1d):anomalies(sum:app.service.latency{platform:macos, country:us}.as_count(), \
         'agile', 5, direction='below', seasonality='daily') >= 1"
            .to_string()
    });

    let ast = parse_or_unparsed(&query);

    println!("query:\n  {query}\n");
    println!("explanation:\n  {}\n", explain(&ast));

    let summary = summarize(&ast, &query);
    match serde_json::to_string_pretty(&summary) {
        Ok(json) => println!("summary:\n{json}\n"),
        Err(e) => eprintln!("failed to serialize summary: {e}"),
    }

    match serde_json::to_string_pretty(&ast) {
        Ok(json) => println!("ast:\n{json}"),
        Err(e) => eprintln!("failed to serialize AST: {e}"),
    }
}
