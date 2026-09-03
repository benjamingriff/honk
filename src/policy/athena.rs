use core::fmt;
use core::ops::ControlFlow;

use sqlparser::ast::{
    DescribeAlias, Query, Select, SetExpr, Statement, UtilityOption, Visit, Visitor,
};
use sqlparser::dialect::Dialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Token, TokenWithSpan, Tokenizer};
use thiserror::Error;

pub const MAX_SQL_BYTES: usize = 1024 * 1024;

#[derive(Debug, Default)]
pub struct AthenaDialect;

impl AthenaDialect {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Dialect for AthenaDialect {
    fn is_identifier_start(&self, ch: char) -> bool {
        ch.is_alphabetic() || ch == '_' || ch == '#' || ch == '@'
    }

    fn is_identifier_part(&self, ch: char) -> bool {
        ch.is_alphabetic()
            || ch.is_ascii_digit()
            || ch == '@'
            || ch == '$'
            || ch == '#'
            || ch == '_'
    }

    fn supports_group_by_expr(&self) -> bool {
        true
    }

    fn supports_left_associative_joins_without_parens(&self) -> bool {
        true
    }

    fn supports_explain_with_utility_options(&self) -> bool {
        true
    }

    fn supports_filter_during_aggregation(&self) -> bool {
        true
    }

    fn supports_lambda_functions(&self) -> bool {
        true
    }

    fn supports_nested_comments(&self) -> bool {
        true
    }

    fn supports_values_as_table_factor(&self) -> bool {
        true
    }

    fn supports_parens_around_table_factor(&self) -> bool {
        true
    }

    fn supports_window_clause_named_window_reference(&self) -> bool {
        true
    }

    fn supports_interval_options(&self) -> bool {
        true
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatementKind {
    Select,
    Explain,
    Show,
    Describe,
}

impl fmt::Display for StatementKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Select => "SELECT",
            Self::Explain => "EXPLAIN",
            Self::Show => "SHOW",
            Self::Describe => "DESCRIBE",
        })
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct ValidatedQuery {
    original_sql: Box<str>,
    kind: StatementKind,
}

impl ValidatedQuery {
    /// Parses and classifies one SQL statement under Honk's read-only policy.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError`] for empty or oversized input, parser failures,
    /// multiple statements, unsupported Athena syntax, and every statement or
    /// query feature outside the allowlist.
    pub fn parse(sql: String) -> Result<Self, PolicyError> {
        validate_length(&sql)?;

        let dialect = AthenaDialect::new();
        let tokens = Tokenizer::new(&dialect, &sql)
            .tokenize_with_location()
            .map_err(|_| PolicyError::Parse)?;

        if requests_athena_time_travel(&tokens) {
            return Err(PolicyError::IcebergTimeTravel);
        }

        let mut parser = Parser::new(&dialect).with_tokens_with_locations(tokens);
        let statements = parser.parse_statements().map_err(|_| PolicyError::Parse)?;

        let [statement] = statements.as_slice() else {
            return match statements.len() {
                0 => Err(PolicyError::Empty),
                count => Err(PolicyError::MultipleStatements(count)),
            };
        };

        let kind = classify_statement(statement)?;
        Ok(Self {
            original_sql: sql.into_boxed_str(),
            kind,
        })
    }

    /// Returns the caller's SQL exactly as supplied.
    #[must_use]
    pub fn sql(&self) -> &str {
        &self.original_sql
    }

    /// Returns the validated top-level statement class.
    #[must_use]
    pub const fn kind(&self) -> StatementKind {
        self.kind
    }
}

impl fmt::Debug for ValidatedQuery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ValidatedQuery")
            .field("kind", &self.kind)
            .field("sql_bytes", &self.original_sql.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum PolicyError {
    #[error("SQL input contains no statement")]
    Empty,

    #[error("SQL input exceeds Honk's {limit}-byte limit")]
    InputTooLarge { limit: usize },

    #[error("SQL could not be parsed by Honk's Athena parser")]
    Parse,

    #[error("SQL input contains {0} statements; Honk accepts exactly one")]
    MultipleStatements(usize),

    #[error(
        "Athena Iceberg time travel is not supported yet; FOR TIMESTAMP AS OF and FOR VERSION AS OF are rejected locally"
    )]
    IcebergTimeTravel,

    #[error(
        "SQL policy rejected {0}; only SELECT, EXPLAIN of SELECT, SHOW, and DESCRIBE are allowed"
    )]
    StatementNotAllowed(&'static str),

    #[error("SQL policy rejected query feature: {0}")]
    QueryFeatureNotAllowed(&'static str),

    #[error("SQL policy rejected EXPLAIN ANALYZE because it executes the query")]
    ExplainAnalyze,

    #[error("SQL policy rejected an unsupported EXPLAIN option")]
    ExplainOptionNotAllowed,
}

fn validate_length(sql: &str) -> Result<(), PolicyError> {
    if sql.len() > MAX_SQL_BYTES {
        Err(PolicyError::InputTooLarge {
            limit: MAX_SQL_BYTES,
        })
    } else {
        Ok(())
    }
}

fn classify_statement(statement: &Statement) -> Result<StatementKind, PolicyError> {
    match statement {
        Statement::Query(query) => {
            validate_query(query)?;
            Ok(StatementKind::Select)
        }
        Statement::Explain {
            describe_alias: DescribeAlias::Explain,
            analyze,
            verbose,
            query_plan,
            estimate,
            statement,
            options,
            ..
        } => {
            if *analyze || explain_options_request_analysis(options.as_deref()) {
                return Err(PolicyError::ExplainAnalyze);
            }
            if *verbose || *query_plan || *estimate {
                return Err(PolicyError::ExplainOptionNotAllowed);
            }
            validate_explain_options(options.as_deref())?;
            let Statement::Query(query) = statement.as_ref() else {
                return Err(PolicyError::StatementNotAllowed(statement_kind(statement)));
            };
            validate_query(query)?;
            Ok(StatementKind::Explain)
        }
        Statement::ExplainTable {
            describe_alias: DescribeAlias::Describe | DescribeAlias::Desc,
            ..
        } => Ok(StatementKind::Describe),
        Statement::ShowFunctions { .. }
        | Statement::ShowVariable { .. }
        | Statement::ShowStatus { .. }
        | Statement::ShowVariables { .. }
        | Statement::ShowCreate { .. }
        | Statement::ShowColumns { .. }
        | Statement::ShowCatalogs { .. }
        | Statement::ShowDatabases { .. }
        | Statement::ShowProcessList { .. }
        | Statement::ShowSchemas { .. }
        | Statement::ShowCharset(_)
        | Statement::ShowObjects(_)
        | Statement::ShowTables { .. }
        | Statement::ShowViews { .. }
        | Statement::ShowCollation { .. } => Ok(StatementKind::Show),
        _ => Err(PolicyError::StatementNotAllowed(statement_kind(statement))),
    }
}

fn validate_query(query: &Query) -> Result<(), PolicyError> {
    let mut visitor = ReadOnlyQueryVisitor;
    match query.visit(&mut visitor) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(rejection) => Err(rejection),
    }
}

struct ReadOnlyQueryVisitor;

impl Visitor for ReadOnlyQueryVisitor {
    type Break = PolicyError;

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        if !query.locks.is_empty() {
            return ControlFlow::Break(PolicyError::QueryFeatureNotAllowed("row-locking clause"));
        }
        if query.settings.is_some() {
            return ControlFlow::Break(PolicyError::QueryFeatureNotAllowed(
                "query settings clause",
            ));
        }
        if query.format_clause.is_some() {
            return ControlFlow::Break(PolicyError::QueryFeatureNotAllowed("query format clause"));
        }
        if query.for_clause.is_some() {
            return ControlFlow::Break(PolicyError::QueryFeatureNotAllowed("output FOR clause"));
        }
        if !query.pipe_operators.is_empty() {
            return ControlFlow::Break(PolicyError::QueryFeatureNotAllowed("pipe operator"));
        }
        if let Err(rejection) = validate_set_expr(&query.body) {
            return ControlFlow::Break(rejection);
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<Self::Break> {
        if select.into.is_some() {
            return ControlFlow::Break(PolicyError::QueryFeatureNotAllowed("SELECT INTO"));
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<Self::Break> {
        match statement {
            Statement::Query(_) => ControlFlow::Continue(()),
            _ => ControlFlow::Break(PolicyError::StatementNotAllowed(statement_kind(statement))),
        }
    }
}

fn validate_set_expr(expression: &SetExpr) -> Result<(), PolicyError> {
    match expression {
        SetExpr::Select(_) | SetExpr::Query(_) => Ok(()),
        SetExpr::SetOperation { left, right, .. } => {
            validate_set_expr(left)?;
            validate_set_expr(right)
        }
        SetExpr::Values(_) => Err(PolicyError::QueryFeatureNotAllowed("VALUES query")),
        SetExpr::Table(_) => Err(PolicyError::QueryFeatureNotAllowed("TABLE query")),
        SetExpr::Insert(statement)
        | SetExpr::Update(statement)
        | SetExpr::Delete(statement)
        | SetExpr::Merge(statement) => {
            Err(PolicyError::StatementNotAllowed(statement_kind(statement)))
        }
    }
}

fn explain_options_request_analysis(options: Option<&[UtilityOption]>) -> bool {
    options.is_some_and(|options| {
        options
            .iter()
            .any(|option| option.name.value.eq_ignore_ascii_case("analyze"))
    })
}

fn validate_explain_options(options: Option<&[UtilityOption]>) -> Result<(), PolicyError> {
    for option in options.unwrap_or_default() {
        if !option.name.value.eq_ignore_ascii_case("format")
            && !option.name.value.eq_ignore_ascii_case("type")
        {
            return Err(PolicyError::ExplainOptionNotAllowed);
        }
    }
    Ok(())
}

fn requests_athena_time_travel(tokens: &[TokenWithSpan]) -> bool {
    let significant = tokens
        .iter()
        .filter(|token| !matches!(token.token, Token::Whitespace(_) | Token::EOF))
        .collect::<Vec<_>>();

    significant.windows(4).any(|window| {
        is_unquoted_word(&window[0].token, "for")
            && (is_unquoted_word(&window[1].token, "timestamp")
                || is_unquoted_word(&window[1].token, "version"))
            && is_unquoted_word(&window[2].token, "as")
            && is_unquoted_word(&window[3].token, "of")
    })
}

fn is_unquoted_word(token: &Token, expected: &str) -> bool {
    matches!(
        token,
        Token::Word(word)
            if word.quote_style.is_none() && word.value.eq_ignore_ascii_case(expected)
    )
}

fn statement_kind(statement: &Statement) -> &'static str {
    match statement {
        Statement::Analyze(_) => "ANALYZE",
        Statement::Set(_) => "SET",
        Statement::Truncate(_) => "TRUNCATE",
        Statement::Msck(_) => "MSCK REPAIR",
        Statement::Query(_) => "query",
        Statement::Insert(_) => "INSERT",
        Statement::Call(_) => "CALL",
        Statement::Update(_) => "UPDATE",
        Statement::Delete(_) => "DELETE",
        Statement::CreateView(_) | Statement::CreateTable(_) => "CREATE",
        Statement::AlterTable(_) | Statement::AlterView { .. } => "ALTER",
        Statement::Drop { .. } => "DROP",
        Statement::Use(_) => "USE",
        Statement::StartTransaction { .. } => "START TRANSACTION",
        Statement::Commit { .. } => "COMMIT",
        Statement::Grant(_) => "GRANT",
        Statement::Deallocate { .. } => "DEALLOCATE",
        Statement::Execute { .. } => "EXECUTE",
        Statement::Prepare { .. } => "PREPARE",
        Statement::Explain { .. } => "EXPLAIN",
        Statement::Merge(_) => "MERGE",
        Statement::Unload { .. } => "UNLOAD",
        Statement::OptimizeTable { .. } => "OPTIMIZE",
        Statement::Vacuum(_) => "VACUUM",
        _ => "unclassified statement",
    }
}
