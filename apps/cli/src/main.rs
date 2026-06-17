//! `ddquery` — a thin command-line front-end over [`ddquery_core`].
//!
//! Reads a Datadog monitor query from the first argument (or stdin) and prints
//! its plain-language explanation, structured summary, and AST. With
//! `--json`, only the machine-readable bundle (`ast` + `summary` +
//! `explanation`) is printed. A query that fails to parse still echoes its raw
//! form rather than producing zero output.

use std::io::{self, Read, Write};

use anyhow::{Context, Result};
use ddquery_core::{MonitorQuery, explain, parse_or_unparsed, summarize};
use serde::Serialize;

/// The machine-readable bundle printed under `--json`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Explained {
    ast: MonitorQuery,
    summary: ddquery_core::QuerySummary,
    explanation: String,
}

fn main() -> Result<()> {
    let mut json = false;
    let mut query: Option<String> = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--json" => json = true,
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            _ => query = Some(arg),
        }
    }

    let query = match query {
        Some(q) => q,
        None => read_stdin().context("failed to read query from stdin")?,
    };
    let query = query.trim();
    if query.is_empty() {
        print_help();
        std::process::exit(2);
    }

    let ast = parse_or_unparsed(query);
    let summary = summarize(&ast, query);
    let explanation = explain(&ast);

    let stdout = io::stdout();
    let mut out = stdout.lock();
    if json {
        let bundle = Explained {
            ast,
            summary,
            explanation,
        };
        let rendered =
            serde_json::to_string_pretty(&bundle).context("failed to serialize output")?;
        writeln!(out, "{rendered}")?;
    } else {
        writeln!(out, "query:\n  {query}\n")?;
        writeln!(out, "explanation:\n  {explanation}\n")?;
        let rendered =
            serde_json::to_string_pretty(&summary).context("failed to serialize summary")?;
        writeln!(out, "summary:\n{rendered}")?;
    }
    Ok(())
}

fn read_stdin() -> Result<String> {
    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}

fn print_help() {
    eprintln!(
        "ddquery — explain a Datadog monitor query\n\n\
         USAGE:\n    \
         ddquery [--json] \"<query>\"\n    \
         echo \"<query>\" | ddquery [--json]\n\n\
         OPTIONS:\n    \
         --json    print the AST + summary + explanation as JSON\n    \
         -h        show this help"
    );
}
