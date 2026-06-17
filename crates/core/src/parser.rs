//! Hand-written recursive-descent parser with a small Pratt loop for
//! arithmetic precedence.
//!
//! The pipeline is `dispatch → recursive-descent → typed AST`. There is no
//! parser-generator dependency: the grammar is small and stable, and a
//! generator would add a build-time codegen step. Parsing is a pure function
//! with no I/O. Recursion depth is bounded ([`MAX_DEPTH`]) so pathologically
//! nested input is rejected before the stack grows.

use crate::{
    ast::{
        ArithOp, BoolOp, ChangeKind, CheckQuery, CmpOp, CompositeExpr, Condition, Filter, FuncKind,
        FuncParam, MetricExpr, MetricQuery, Modifier, MonitorQuery, MonitorRef, ParamValue, Scalar,
        SearchQuery, SearchSource, Series, SloQuery, SpaceAgg, TimeAgg, Window,
    },
    cursor::Cursor,
    error::ParseError,
};

/// Maximum expression/recursion depth. Anything deeper is rejected as
/// pathological input rather than risking stack growth.
const MAX_DEPTH: usize = 128;

/// Parse a Datadog monitor query into a typed AST.
///
/// # Errors
///
/// Returns a [`ParseError`] (byte offset + reason) when the query does not
/// match any known dialect. For a non-failing entry point that degrades to
/// [`MonitorQuery::Unparsed`], use [`crate::parse_or_unparsed`].
pub fn parse(query: &str) -> Result<MonitorQuery, ParseError> {
    if query.trim().is_empty() {
        return Err(ParseError::new(0, "empty query"));
    }
    let mut parser = Parser::new(query);
    let parsed = parser.parse_query()?;
    parser.cursor.skip_ws();
    if !parser.cursor.is_done() {
        return Err(parser.cursor.error("unexpected trailing input"));
    }
    Ok(parsed)
}

/// A char predicate for identifier and metric-name-segment characters
/// (`[A-Za-z0-9_]`).
fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

struct Parser<'a> {
    cursor: Cursor<'a>,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            cursor: Cursor::new(input),
        }
    }

    /// Dispatch to a dialect parser based on the leading token.
    fn parse_query(&mut self) -> Result<MonitorQuery, ParseError> {
        self.cursor.skip_ws();
        match self.cursor.peek() {
            Some('"') => self.parse_check().map(MonitorQuery::ServiceCheck),
            Some(c) if c.is_ascii_digit() || c == '!' || c == '(' => {
                self.parse_composite(0).map(MonitorQuery::Composite)
            }
            Some(_) => {
                let head = self.peek_ident();
                match head.as_str() {
                    "error_budget" => self.parse_slo().map(MonitorQuery::Slo),
                    "logs" | "events" | "rum" | "error-tracking" => {
                        self.parse_search().map(MonitorQuery::Search)
                    }
                    "avg" | "sum" | "min" | "max" | "count" | "change" | "pct_change" => {
                        self.parse_metric().map(MonitorQuery::Metric)
                    }
                    "" => Err(self.cursor.error("unrecognized query")),
                    other => Err(self
                        .cursor
                        .error(format!("unrecognized query head `{other}`"))),
                }
            }
            None => Err(self.cursor.error("empty query")),
        }
    }

    /// Peek the leading identifier (including `-` for `error-tracking`) without
    /// consuming it.
    fn peek_ident(&self) -> String {
        let mut probe = self.cursor.clone();
        probe.take_while(|c| is_ident(c) || c == '-').to_string()
    }

    fn check_depth(&self, depth: usize) -> Result<(), ParseError> {
        if depth > MAX_DEPTH {
            Err(self.cursor.error("maximum nesting depth exceeded"))
        } else {
            Ok(())
        }
    }

    // ----- metric dialect ------------------------------------------------

    fn parse_metric(&mut self) -> Result<MetricQuery, ParseError> {
        let head = self.take_ident()?;
        if let Some(kind) = ChangeKind::from_token(&head) {
            return self.parse_change_metric(kind);
        }
        let time_aggregation = TimeAgg::from_token(&head).ok_or_else(|| {
            self.cursor
                .error(format!("invalid time aggregation `{head}`"))
        })?;
        self.cursor.expect("(")?;
        let window = self.take_window()?;
        self.cursor.expect(")")?;
        self.cursor.expect(":")?;
        let expr = self.parse_arith(0, 0)?;
        let condition = self.parse_condition()?;
        Ok(MetricQuery {
            time_aggregation,
            window,
            expr,
            condition,
        })
    }

    /// `change(avg(last_5m),last_1h): expr op threshold`.
    fn parse_change_metric(&mut self, kind: ChangeKind) -> Result<MetricQuery, ParseError> {
        self.cursor.expect("(")?;
        let inner = self.take_ident()?;
        let inner_agg = TimeAgg::from_token(&inner).ok_or_else(|| {
            self.cursor
                .error(format!("invalid time aggregation `{inner}`"))
        })?;
        self.cursor.expect("(")?;
        let window = self.take_window()?;
        self.cursor.expect(")")?;
        self.cursor.expect(",")?;
        let shift = self.take_window()?;
        self.cursor.expect(")")?;
        self.cursor.expect(":")?;
        let inner_expr = self.parse_arith(0, 0)?;
        let condition = self.parse_condition()?;
        let expr = MetricExpr::Change {
            kind,
            inner_agg,
            shift,
            arg: Box::new(inner_expr),
        };
        Ok(MetricQuery {
            time_aggregation: inner_agg,
            window,
            expr,
            condition,
        })
    }

    /// Pratt loop: `*`/`/` bind tighter than `+`/`-`.
    fn parse_arith(&mut self, min_bp: u8, depth: usize) -> Result<MetricExpr, ParseError> {
        self.check_depth(depth)?;
        let mut lhs = self.parse_term(depth)?;
        while let Some(op) = self.peek_arith_op() {
            let (lbp, rbp) = binding_power(op);
            if lbp < min_bp {
                break;
            }
            self.cursor.eat(op.as_token());
            let rhs = self.parse_arith(rbp, depth + 1)?;
            lhs = MetricExpr::Arith {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    fn peek_arith_op(&self) -> Option<ArithOp> {
        match self.cursor.peek()? {
            '+' => Some(ArithOp::Add),
            '-' => Some(ArithOp::Sub),
            '*' => Some(ArithOp::Mul),
            '/' => Some(ArithOp::Div),
            _ => None,
        }
    }

    fn parse_term(&mut self, depth: usize) -> Result<MetricExpr, ParseError> {
        self.check_depth(depth)?;
        let c = self
            .cursor
            .peek()
            .ok_or_else(|| self.cursor.error("unexpected end of expression"))?;
        if c == '(' {
            self.cursor.expect("(")?;
            let inner = self.parse_arith(0, depth + 1)?;
            self.cursor.expect(")")?;
            return Ok(inner);
        }
        if c.is_ascii_digit() || c == '+' || c == '-' || c == '.' {
            return Ok(MetricExpr::Scalar(Scalar::new(self.cursor.take_number()?)));
        }
        let id = self.take_ident()?;
        if let Some(func) = FuncKind::from_token(&id)
            && self.cursor.peek() == Some('(')
        {
            return self.parse_function(func, depth);
        }
        if self.cursor.peek() == Some(':') {
            let space_aggregation = SpaceAgg::from_token(&id).ok_or_else(|| {
                self.cursor
                    .error(format!("invalid space aggregation `{id}`"))
            })?;
            self.cursor.expect(":")?;
            return Ok(MetricExpr::Series(self.parse_series(space_aggregation)?));
        }
        if self.cursor.peek() == Some('(') {
            return self.parse_transform(id, depth);
        }
        Err(self
            .cursor
            .error(format!("expected `:` or `(` after `{id}`")))
    }

    fn parse_function(&mut self, name: FuncKind, depth: usize) -> Result<MetricExpr, ParseError> {
        self.cursor.expect("(")?;
        let arg = self.parse_arith(0, depth + 1)?;
        let mut params = Vec::new();
        while self.cursor.eat(",") {
            params.push(self.parse_func_param()?);
        }
        self.cursor.expect(")")?;
        Ok(MetricExpr::Function {
            name,
            arg: Box::new(arg),
            params,
        })
    }

    fn parse_func_param(&mut self) -> Result<FuncParam, ParseError> {
        match self.cursor.peek() {
            Some('\'' | '"') => Ok(FuncParam::positional(ParamValue::Str(
                self.cursor.take_quoted()?,
            ))),
            Some(c) if c.is_ascii_digit() || c == '+' || c == '-' || c == '.' => Ok(
                FuncParam::positional(ParamValue::Number(self.cursor.take_number()?)),
            ),
            Some(_) => {
                let key = self.take_ident()?;
                if self.cursor.eat("=") {
                    Ok(FuncParam::keyword(key, self.parse_param_value()?))
                } else {
                    Ok(FuncParam::positional(bareword_value(&key)))
                }
            }
            None => Err(self.cursor.error("expected a function parameter")),
        }
    }

    fn parse_param_value(&mut self) -> Result<ParamValue, ParseError> {
        match self.cursor.peek() {
            Some('\'' | '"') => Ok(ParamValue::Str(self.cursor.take_quoted()?)),
            Some(c) if c.is_ascii_digit() || c == '+' || c == '-' || c == '.' => {
                Ok(ParamValue::Number(self.cursor.take_number()?))
            }
            Some(_) => Ok(bareword_value(&self.take_ident()?)),
            None => Err(self.cursor.error("expected a parameter value")),
        }
    }

    fn parse_transform(&mut self, name: String, depth: usize) -> Result<MetricExpr, ParseError> {
        self.cursor.expect("(")?;
        let arg = self.parse_arith(0, depth + 1)?;
        let mut args = Vec::new();
        while self.cursor.eat(",") {
            args.push(Scalar::new(self.cursor.take_number()?));
        }
        self.cursor.expect(")")?;
        Ok(MetricExpr::Transform {
            name,
            arg: Box::new(arg),
            args,
        })
    }

    fn parse_series(&mut self, space_aggregation: SpaceAgg) -> Result<Series, ParseError> {
        let metric = self.take_metric_name()?;
        let filter = if self.cursor.peek() == Some('{') {
            self.cursor.expect("{")?;
            let filters = self.parse_filter_list();
            self.cursor.expect("}")?;
            filters
        } else {
            Vec::new()
        };
        let group_by = self.parse_group_by()?;
        let modifiers = self.parse_modifiers()?;
        Ok(Series {
            space_aggregation,
            metric,
            filter,
            group_by,
            modifiers,
        })
    }

    fn parse_filter_list(&mut self) -> Vec<Filter> {
        let mut filters = Vec::new();
        loop {
            self.skip_filter_seps();
            if matches!(self.cursor.peek(), Some('}') | None) {
                break;
            }
            let token = self
                .cursor
                .take_while(|c| c != ',' && c != '}' && !c.is_whitespace());
            if token.is_empty() {
                break;
            }
            filters.push(classify_filter(token));
        }
        filters
    }

    /// Commas and runs of whitespace are equivalent filter separators.
    fn skip_filter_seps(&mut self) {
        loop {
            self.cursor.skip_ws();
            if !self.cursor.eat(",") {
                break;
            }
        }
    }

    fn parse_group_by(&mut self) -> Result<Vec<String>, ParseError> {
        self.cursor.skip_ws();
        let rest = self.cursor.rest();
        let is_by = rest
            .strip_prefix("by")
            .is_some_and(|after| after.starts_with(|c: char| c.is_whitespace() || c == '{'));
        if !is_by {
            return Ok(Vec::new());
        }
        self.cursor.eat("by");
        self.cursor.expect("{")?;
        let mut tags = Vec::new();
        loop {
            self.skip_filter_seps();
            if self.cursor.peek() == Some('}') || self.cursor.peek().is_none() {
                break;
            }
            let tag = self
                .cursor
                .take_while(|c| c != ',' && c != '}' && !c.is_whitespace());
            if tag.is_empty() {
                break;
            }
            tags.push(tag.to_string());
        }
        self.cursor.expect("}")?;
        Ok(tags)
    }

    fn parse_modifiers(&mut self) -> Result<Vec<Modifier>, ParseError> {
        let mut modifiers = Vec::new();
        loop {
            self.cursor.skip_ws();
            // A `.` immediately following the series begins a modifier. Avoid
            // skipping whitespace so arithmetic like `a . b` is never read as
            // a modifier (which Datadog never emits anyway).
            if !self.cursor.rest().starts_with('.') {
                break;
            }
            self.cursor.expect(".")?;
            let name = self.take_ident()?;
            self.cursor.expect("(")?;
            let args = self.parse_string_args()?;
            modifiers.push(Modifier { name, args });
        }
        Ok(modifiers)
    }

    // ----- search dialect ------------------------------------------------

    fn parse_search(&mut self) -> Result<SearchQuery, ParseError> {
        let source = self.take_source()?;
        self.cursor.expect("(")?;
        let raw_search = self.cursor.take_quoted()?;
        self.cursor.expect(")")?;

        let mut index = None;
        let mut rollup_method = None;
        let mut rollup_arg = None;
        let mut group_by = Vec::new();
        let mut last = None;

        while self.cursor.eat(".") {
            let method = self.take_ident()?;
            self.cursor.expect("(")?;
            let mut args = self.parse_string_args()?;
            match method.as_str() {
                "index" => index = args.into_iter().next().filter(|s| !s.is_empty()),
                "rollup" => {
                    let mut it = args.into_iter();
                    rollup_method = it.next();
                    rollup_arg = it.next();
                }
                "by" => group_by = args,
                "last" => last = args.drain(..).next(),
                other => {
                    return Err(self
                        .cursor
                        .error(format!("unknown search method `{other}`")));
                }
            }
        }

        let rollup_method = rollup_method
            .ok_or_else(|| self.cursor.error("search query missing `.rollup(...)`"))?;
        let last = last.ok_or_else(|| self.cursor.error("search query missing `.last(...)`"))?;
        let condition = self.parse_condition()?;
        Ok(SearchQuery {
            source,
            raw_search,
            index,
            rollup_method,
            rollup_arg,
            group_by,
            last,
            condition,
        })
    }

    fn take_source(&mut self) -> Result<SearchSource, ParseError> {
        let id = self.cursor.take_while(|c| is_ident(c) || c == '-');
        match id {
            "logs" => Ok(SearchSource::Logs),
            "events" => Ok(SearchSource::Events),
            "rum" => Ok(SearchSource::Rum),
            "error-tracking" => Ok(SearchSource::ErrorTracking),
            other => Err(self
                .cursor
                .error(format!("invalid search source `{other}`"))),
        }
    }

    // ----- service-check dialect ----------------------------------------

    fn parse_check(&mut self) -> Result<CheckQuery, ParseError> {
        let check = self.cursor.take_quoted()?;
        let mut over = Vec::new();
        let mut by = Vec::new();
        let mut last = None;
        let mut saw_count = false;

        while self.cursor.eat(".") {
            let method = self.take_ident()?;
            self.cursor.expect("(")?;
            match method.as_str() {
                "over" => over = self.parse_string_args()?,
                "by" => by = self.parse_string_args()?,
                "last" => {
                    let n = self.cursor.take_number()?;
                    self.cursor.expect(")")?;
                    last = Some(checked_u32(n).ok_or_else(|| {
                        self.cursor
                            .error("`.last(n)` must be a non-negative integer")
                    })?);
                }
                "count_by_status" => {
                    self.parse_string_args()?;
                    saw_count = true;
                }
                other => {
                    return Err(self.cursor.error(format!("unknown check method `{other}`")));
                }
            }
        }

        let last = last.ok_or_else(|| self.cursor.error("check query missing `.last(n)`"))?;
        if !saw_count {
            return Err(self
                .cursor
                .error("check query missing `.count_by_status()`"));
        }
        Ok(CheckQuery {
            check,
            over,
            by,
            last,
        })
    }

    // ----- SLO dialect ---------------------------------------------------

    fn parse_slo(&mut self) -> Result<SloQuery, ParseError> {
        self.cursor.expect("error_budget")?;
        self.cursor.expect("(")?;
        let id = self.cursor.take_quoted()?;
        self.cursor.expect(")")?;
        self.cursor.expect(".")?;
        let method = self.take_ident()?;
        if method != "over" {
            return Err(self
                .cursor
                .error(format!("expected `.over(...)`, found `.{method}`")));
        }
        self.cursor.expect("(")?;
        let over = self.cursor.take_quoted()?;
        self.cursor.expect(")")?;
        let condition = self.parse_condition()?;
        Ok(SloQuery {
            id,
            over,
            condition,
        })
    }

    // ----- composite dialect --------------------------------------------

    fn parse_composite(&mut self, depth: usize) -> Result<CompositeExpr, ParseError> {
        self.parse_composite_or(depth)
    }

    fn parse_composite_or(&mut self, depth: usize) -> Result<CompositeExpr, ParseError> {
        self.check_depth(depth)?;
        let mut lhs = self.parse_composite_and(depth)?;
        while self.cursor.eat("||") {
            let rhs = self.parse_composite_and(depth + 1)?;
            lhs = CompositeExpr::Binary {
                op: BoolOp::Or,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    fn parse_composite_and(&mut self, depth: usize) -> Result<CompositeExpr, ParseError> {
        self.check_depth(depth)?;
        let mut lhs = self.parse_composite_unary(depth)?;
        while self.cursor.eat("&&") {
            let rhs = self.parse_composite_unary(depth + 1)?;
            lhs = CompositeExpr::Binary {
                op: BoolOp::And,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    fn parse_composite_unary(&mut self, depth: usize) -> Result<CompositeExpr, ParseError> {
        self.check_depth(depth)?;
        if self.cursor.eat("!") {
            return Ok(CompositeExpr::Not(Box::new(
                self.parse_composite_unary(depth + 1)?,
            )));
        }
        if self.cursor.eat("(") {
            let inner = self.parse_composite_or(depth + 1)?;
            self.cursor.expect(")")?;
            return Ok(inner);
        }
        let id = self.cursor.take_while(|c| c.is_ascii_digit());
        if id.is_empty() {
            return Err(self.cursor.error("expected a monitor id"));
        }
        Ok(CompositeExpr::Ref(MonitorRef { id: id.to_string() }))
    }

    // ----- shared helpers ------------------------------------------------

    fn parse_condition(&mut self) -> Result<Condition, ParseError> {
        let operator = self.parse_cmp_op()?;
        let critical = Scalar::new(self.cursor.take_number()?);
        Ok(Condition {
            operator,
            critical,
            critical_recovery: None,
            warning: None,
            warning_recovery: None,
        })
    }

    fn parse_cmp_op(&mut self) -> Result<CmpOp, ParseError> {
        for op in [CmpOp::Ge, CmpOp::Le, CmpOp::Eq, CmpOp::Gt, CmpOp::Lt] {
            if self.cursor.eat(op.as_token()) {
                return Ok(op);
            }
        }
        Err(self.cursor.error("expected a comparison operator"))
    }

    /// Parse comma-separated quoted-or-bareword arguments through the closing
    /// `)`. Assumes the opening `(` has already been consumed.
    fn parse_string_args(&mut self) -> Result<Vec<String>, ParseError> {
        let mut out = Vec::new();
        if self.cursor.eat(")") {
            return Ok(out);
        }
        loop {
            self.cursor.skip_ws();
            let value = match self.cursor.peek() {
                Some('\'' | '"') => self.cursor.take_quoted()?,
                _ => self
                    .cursor
                    .take_while(|c| c != ',' && c != ')')
                    .trim()
                    .to_string(),
            };
            out.push(value);
            if self.cursor.eat(",") {
                continue;
            }
            self.cursor.expect(")")?;
            break;
        }
        Ok(out)
    }

    fn take_ident(&mut self) -> Result<String, ParseError> {
        let id = self.cursor.take_while(is_ident);
        if id.is_empty() {
            Err(self.cursor.error("expected an identifier"))
        } else {
            Ok(id.to_string())
        }
    }

    fn take_window(&mut self) -> Result<Window, ParseError> {
        let raw = self.cursor.take_while(is_ident);
        Window::parse(raw).ok_or_else(|| self.cursor.error(format!("invalid window `{raw}`")))
    }

    /// Read a dotted metric name (`a.b.c`), stopping before a `.modifier(`.
    fn take_metric_name(&mut self) -> Result<String, ParseError> {
        let first = self.cursor.take_while(is_ident);
        if first.is_empty() {
            return Err(self.cursor.error("expected a metric name"));
        }
        let mut name = first.to_string();
        loop {
            if !self.cursor.rest().starts_with('.') {
                break;
            }
            // Look ahead: a `.ident(` is a modifier, not a name segment.
            let mut probe = self.cursor.clone();
            probe.eat(".");
            let seg = probe.take_while(is_ident);
            if seg.is_empty() || probe.rest().starts_with('(') {
                break;
            }
            self.cursor.eat(".");
            let committed = self.cursor.take_while(is_ident);
            name.push('.');
            name.push_str(committed);
        }
        Ok(name)
    }
}

/// Operator binding power: `*`/`/` bind tighter than `+`/`-`.
fn binding_power(op: ArithOp) -> (u8, u8) {
    match op {
        ArithOp::Add | ArithOp::Sub => (1, 2),
        ArithOp::Mul | ArithOp::Div => (3, 4),
    }
}

/// Classify a raw filter token into a [`Filter`].
fn classify_filter(token: &str) -> Filter {
    if token == "*" {
        return Filter::All;
    }
    let (negated, body) = match token.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, token),
    };
    match body.split_once(':') {
        Some((key, value)) => Filter::Tag {
            negated,
            key: key.to_string(),
            value: value.to_string(),
        },
        None => Filter::Glob(token.to_string()),
    }
}

/// Interpret a bareword parameter value (`true`/`false` → bool, else string).
fn bareword_value(word: &str) -> ParamValue {
    match word {
        "true" => ParamValue::Bool(true),
        "false" => ParamValue::Bool(false),
        other => ParamValue::Str(other.to_string()),
    }
}

/// Convert a parsed `f64` to a `u32` if it is a non-negative whole number.
fn checked_u32(value: f64) -> Option<u32> {
    if value.is_finite() && value.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(&value) {
        Some(value as u32)
    } else {
        None
    }
}
