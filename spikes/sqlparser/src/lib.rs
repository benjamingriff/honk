use core::ops::ControlFlow;

use sqlparser::ast::{
    DescribeAlias, Query, Select, SetExpr, Statement, UtilityOption, Visit, Visitor,
};
use sqlparser::dialect::Dialect;
use sqlparser::parser::Parser;

#[derive(Debug, Default)]
pub struct AthenaDialect;

impl AthenaDialect {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllowedKind {
    Query,
    Explain,
    Show,
    Describe,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    Empty,
    Parse(String),
    MultipleStatements(usize),
    StatementNotAllowed(String),
    QueryFeatureNotAllowed(&'static str),
    ExplainAnalyze,
    ExplainOptionNotAllowed(String),
}

pub fn validate(sql: &str) -> Result<AllowedKind, Rejection> {
    if sql.trim().is_empty() {
        return Err(Rejection::Empty);
    }

    let statements = Parser::parse_sql(&AthenaDialect::new(), sql)
        .map_err(|error| Rejection::Parse(error.to_string()))?;

    if statements.len() != 1 {
        return Err(Rejection::MultipleStatements(statements.len()));
    }

    classify_statement(&statements[0])
}

fn classify_statement(statement: &Statement) -> Result<AllowedKind, Rejection> {
    match statement {
        Statement::Query(query) => {
            validate_query(query)?;
            Ok(AllowedKind::Query)
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
                return Err(Rejection::ExplainAnalyze);
            }
            if *verbose || *query_plan || *estimate {
                return Err(Rejection::ExplainOptionNotAllowed(
                    "unsupported EXPLAIN modifier".to_owned(),
                ));
            }
            validate_explain_options(options.as_deref())?;
            let Statement::Query(query) = statement.as_ref() else {
                return Err(Rejection::StatementNotAllowed(format!(
                    "EXPLAIN of {}",
                    statement_kind(statement)
                )));
            };
            validate_query(query)?;
            Ok(AllowedKind::Explain)
        }
        Statement::ExplainTable {
            describe_alias: DescribeAlias::Describe | DescribeAlias::Desc,
            ..
        } => Ok(AllowedKind::Describe),
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
        | Statement::ShowCollation { .. } => Ok(AllowedKind::Show),
        _ => Err(Rejection::StatementNotAllowed(statement_kind(statement))),
    }
}

fn validate_query(query: &Query) -> Result<(), Rejection> {
    let mut visitor = ReadOnlyQueryVisitor;
    match query.visit(&mut visitor) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(rejection) => Err(rejection),
    }
}

struct ReadOnlyQueryVisitor;

impl Visitor for ReadOnlyQueryVisitor {
    type Break = Rejection;

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        if !query.locks.is_empty() {
            return ControlFlow::Break(Rejection::QueryFeatureNotAllowed("row-locking clause"));
        }
        if query.settings.is_some() {
            return ControlFlow::Break(Rejection::QueryFeatureNotAllowed("query settings clause"));
        }
        if query.format_clause.is_some() {
            return ControlFlow::Break(Rejection::QueryFeatureNotAllowed("query format clause"));
        }
        if query.for_clause.is_some() {
            return ControlFlow::Break(Rejection::QueryFeatureNotAllowed("output FOR clause"));
        }
        if !query.pipe_operators.is_empty() {
            return ControlFlow::Break(Rejection::QueryFeatureNotAllowed("pipe operator"));
        }
        if let Err(rejection) = validate_set_expr(&query.body) {
            return ControlFlow::Break(rejection);
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<Self::Break> {
        if select.into.is_some() {
            return ControlFlow::Break(Rejection::QueryFeatureNotAllowed("SELECT INTO"));
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_statement(&mut self, statement: &Statement) -> ControlFlow<Self::Break> {
        match statement {
            Statement::Query(_) => ControlFlow::Continue(()),
            _ => ControlFlow::Break(Rejection::StatementNotAllowed(format!(
                "nested {}",
                statement_kind(statement)
            ))),
        }
    }
}

fn validate_set_expr(expression: &SetExpr) -> Result<(), Rejection> {
    match expression {
        SetExpr::Select(_) | SetExpr::Query(_) => Ok(()),
        SetExpr::SetOperation { left, right, .. } => {
            validate_set_expr(left)?;
            validate_set_expr(right)
        }
        SetExpr::Values(_) => Err(Rejection::QueryFeatureNotAllowed("VALUES query")),
        SetExpr::Table(_) => Err(Rejection::QueryFeatureNotAllowed("TABLE query")),
        SetExpr::Insert(statement)
        | SetExpr::Update(statement)
        | SetExpr::Delete(statement)
        | SetExpr::Merge(statement) => Err(Rejection::StatementNotAllowed(format!(
            "query-bodied {}",
            statement_kind(statement)
        ))),
    }
}

fn explain_options_request_analysis(options: Option<&[UtilityOption]>) -> bool {
    options.is_some_and(|options| {
        options
            .iter()
            .any(|option| option.name.value.eq_ignore_ascii_case("analyze"))
    })
}

fn validate_explain_options(options: Option<&[UtilityOption]>) -> Result<(), Rejection> {
    for option in options.unwrap_or_default() {
        if !option.name.value.eq_ignore_ascii_case("format")
            && !option.name.value.eq_ignore_ascii_case("type")
        {
            return Err(Rejection::ExplainOptionNotAllowed(
                option.name.value.clone(),
            ));
        }
    }
    Ok(())
}

fn statement_kind(statement: &Statement) -> String {
    let debug = format!("{statement:?}");
    debug
        .split([' ', '(', '{'])
        .next()
        .unwrap_or("unknown statement")
        .to_owned()
}
