mod api;
mod sdk;

use std::fmt;
use std::io;
use std::path::Path;

use thiserror::Error;

use self::api::{ApiError, Catalog, Database, MetadataApi, Table};
use crate::auth::VerifiedSession;
use crate::config::Connection;
use crate::output::{ColumnSpec, Formatter, OutputPlan, rename_columns};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    Catalogs,
    Databases,
    Tables,
    Describe,
}

impl fmt::Display for Operation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Catalogs => "list catalogs",
            Self::Databases => "list databases",
            Self::Tables => "list tables",
            Self::Describe => "describe table",
        })
    }
}

#[derive(Debug, Error)]
pub enum MetadataError {
    #[error(
        "AWS credentials expired while attempting to {operation}; refresh the session and try again"
    )]
    CredentialsExpired { operation: Operation },

    #[error("AWS denied permission to {operation} in catalog {catalog:?}")]
    AccessDenied {
        operation: Operation,
        catalog: String,
    },

    #[error("table {database:?}.{table:?} was not found in catalog {catalog:?}")]
    TableNotFound {
        catalog: String,
        database: String,
        table: String,
    },

    #[error(
        "the AWS metadata service failed while attempting to {operation} in catalog {catalog:?}"
    )]
    ProviderFailed {
        operation: Operation,
        catalog: String,
    },

    #[error(
        "the AWS metadata service returned an incomplete response while attempting to {operation}"
    )]
    MalformedResponse { operation: Operation },

    #[error("cannot prepare metadata output: {source}")]
    PrepareOutput {
        #[source]
        source: io::Error,
    },

    #[error("cannot write metadata output: {source}")]
    Output {
        #[source]
        source: io::Error,
    },
}

impl MetadataError {
    pub(crate) const fn exit_code(&self) -> u8 {
        match self {
            Self::CredentialsExpired { .. } => 3,
            Self::PrepareOutput { .. } | Self::Output { .. } => 5,
            Self::AccessDenied { .. }
            | Self::TableNotFound { .. }
            | Self::ProviderFailed { .. }
            | Self::MalformedResponse { .. } => 4,
        }
    }
}

pub(crate) async fn run_catalogs(
    connection: &Connection,
    session_name: &str,
    session: &VerifiedSession,
    output_plan: OutputPlan,
    quiet: bool,
) -> Result<(), MetadataError> {
    let client = sdk::SdkMetadata::new(session.credentials_provider(), &connection.region).await;
    run(
        &client,
        Request::Catalogs,
        Context { connection },
        session_name,
        output_plan,
        quiet,
    )
    .await
}

pub(crate) async fn run_databases(
    connection: &Connection,
    session_name: &str,
    session: &VerifiedSession,
    output_plan: OutputPlan,
    quiet: bool,
) -> Result<(), MetadataError> {
    let client = sdk::SdkMetadata::new(session.credentials_provider(), &connection.region).await;
    run(
        &client,
        Request::Databases,
        Context { connection },
        session_name,
        output_plan,
        quiet,
    )
    .await
}

pub(crate) async fn run_tables(
    connection: &Connection,
    session_name: &str,
    session: &VerifiedSession,
    database: &str,
    output_plan: OutputPlan,
    quiet: bool,
) -> Result<(), MetadataError> {
    let client = sdk::SdkMetadata::new(session.credentials_provider(), &connection.region).await;
    run(
        &client,
        Request::Tables { database },
        Context { connection },
        session_name,
        output_plan,
        quiet,
    )
    .await
}

pub(crate) async fn run_describe(
    connection: &Connection,
    session_name: &str,
    session: &VerifiedSession,
    object: &ObjectName,
    output_plan: OutputPlan,
    quiet: bool,
) -> Result<(), MetadataError> {
    let client = sdk::SdkMetadata::new(session.credentials_provider(), &connection.region).await;
    run(
        &client,
        Request::Describe {
            database: &object.database,
            table: &object.table,
        },
        Context { connection },
        session_name,
        output_plan,
        quiet,
    )
    .await
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ObjectName {
    database: String,
    table: String,
}

pub(crate) fn parse_object(
    value: &str,
    default_database: &str,
) -> Result<ObjectName, &'static str> {
    let parts = value.split('.').collect::<Vec<_>>();
    let (database, table) = match parts.as_slice() {
        [table] if !table.trim().is_empty() => (default_database, *table),
        [database, table] if !database.trim().is_empty() && !table.trim().is_empty() => {
            (*database, *table)
        }
        _ => return Err("describe target must be TABLE or DATABASE.TABLE"),
    };
    Ok(ObjectName {
        database: database.to_owned(),
        table: table.to_owned(),
    })
}

struct Context<'a> {
    connection: &'a Connection,
}

enum Request<'a> {
    Catalogs,
    Databases,
    Tables { database: &'a str },
    Describe { database: &'a str, table: &'a str },
}

impl Request<'_> {
    const fn operation(&self) -> Operation {
        match self {
            Self::Catalogs => Operation::Catalogs,
            Self::Databases => Operation::Databases,
            Self::Tables { .. } => Operation::Tables,
            Self::Describe { .. } => Operation::Describe,
        }
    }

    fn database(&self) -> Option<&str> {
        match self {
            Self::Tables { database } | Self::Describe { database, .. } => Some(database),
            Self::Catalogs | Self::Databases => None,
        }
    }
}

async fn run(
    api: &dyn MetadataApi,
    request: Request<'_>,
    context: Context<'_>,
    session_name: &str,
    output_plan: OutputPlan,
    quiet: bool,
) -> Result<(), MetadataError> {
    let operation = request.operation();
    let database = request.database().map(str::to_owned);
    let mut output = output_plan
        .prepare()
        .map_err(|source| MetadataError::PrepareOutput { source })?;
    let (specs, rows) = discover(api, &request, &context).await?;
    let (columns, _) = rename_columns(&specs);
    let format = output.format;
    let table_width = output.table_width;
    let row_count = {
        let mut formatter = Formatter::begin(output.writer(), format, &columns, table_width)
            .map_err(|source| MetadataError::Output { source })?;
        for row in &rows {
            formatter
                .row(row)
                .map_err(|source| MetadataError::Output { source })?;
        }
        formatter
            .finish()
            .map_err(|source| MetadataError::Output { source })?
    };
    let output_path = output
        .commit()
        .map_err(|source| MetadataError::Output { source })?;
    let stderr = io::stderr();
    write_operations(
        &mut stderr.lock(),
        context.connection,
        session_name,
        operation,
        database.as_deref(),
        row_count,
        output_path.as_deref(),
        quiet,
    )
    .map_err(|source| MetadataError::Output { source })
}

async fn discover(
    api: &dyn MetadataApi,
    request: &Request<'_>,
    context: &Context<'_>,
) -> Result<(Vec<ColumnSpec>, Vec<Vec<Option<String>>>), MetadataError> {
    let connection = context.connection;
    match request {
        Request::Catalogs => {
            let items = all_catalogs(api, &connection.workgroup)
                .await
                .map_err(|error| map_error(error, request, connection, None, None))?;
            if items.iter().any(|item| item.name.is_empty()) {
                return Err(MetadataError::MalformedResponse {
                    operation: Operation::Catalogs,
                });
            }
            Ok((catalog_columns(), catalog_rows(&items)))
        }
        Request::Databases => {
            let items = if is_glue_catalog(&connection.catalog) {
                all_glue_databases(api, &connection.account).await
            } else {
                all_athena_databases(api, &connection.catalog, &connection.workgroup).await
            }
            .map_err(|error| map_error(error, request, connection, None, None))?;
            Ok((
                database_columns(),
                database_rows(&connection.catalog, &items),
            ))
        }
        Request::Tables { database } => {
            let items = if is_glue_catalog(&connection.catalog) {
                all_glue_tables(api, &connection.account, database).await
            } else {
                all_athena_tables(api, &connection.catalog, database, &connection.workgroup).await
            }
            .map_err(|error| map_error(error, request, connection, Some(database), None))?;
            Ok((
                table_columns(),
                table_rows(&connection.catalog, database, &items),
            ))
        }
        Request::Describe { database, table } => {
            let item = if is_glue_catalog(&connection.catalog) {
                api.get_glue_table(&connection.account, database, table)
                    .await
            } else {
                api.get_athena_table(&connection.catalog, database, table, &connection.workgroup)
                    .await
            }
            .map_err(|error| map_error(error, request, connection, Some(database), Some(table)))?;
            Ok((
                describe_columns(),
                describe_rows(&connection.catalog, database, &item),
            ))
        }
    }
}

async fn all_catalogs(api: &dyn MetadataApi, workgroup: &str) -> Result<Vec<Catalog>, ApiError> {
    let mut items = Vec::new();
    let mut token = None;
    loop {
        let page = api.list_catalogs(workgroup, token.as_deref()).await?;
        items.extend(page.items);
        if page.next_token.as_deref().is_none_or(str::is_empty) {
            return Ok(items);
        }
        token = page.next_token;
    }
}

async fn all_glue_databases(
    api: &dyn MetadataApi,
    account: &str,
) -> Result<Vec<Database>, ApiError> {
    let mut items = Vec::new();
    let mut token = None;
    loop {
        let page = api.list_glue_databases(account, token.as_deref()).await?;
        items.extend(page.items);
        if page.next_token.as_deref().is_none_or(str::is_empty) {
            return Ok(items);
        }
        token = page.next_token;
    }
}

async fn all_athena_databases(
    api: &dyn MetadataApi,
    catalog: &str,
    workgroup: &str,
) -> Result<Vec<Database>, ApiError> {
    let mut items = Vec::new();
    let mut token = None;
    loop {
        let page = api
            .list_athena_databases(catalog, workgroup, token.as_deref())
            .await?;
        items.extend(page.items);
        if page.next_token.as_deref().is_none_or(str::is_empty) {
            return Ok(items);
        }
        token = page.next_token;
    }
}

async fn all_glue_tables(
    api: &dyn MetadataApi,
    account: &str,
    database: &str,
) -> Result<Vec<Table>, ApiError> {
    let mut items = Vec::new();
    let mut token = None;
    loop {
        let page = api
            .list_glue_tables(account, database, token.as_deref())
            .await?;
        items.extend(page.items);
        if page.next_token.as_deref().is_none_or(str::is_empty) {
            return Ok(items);
        }
        token = page.next_token;
    }
}

async fn all_athena_tables(
    api: &dyn MetadataApi,
    catalog: &str,
    database: &str,
    workgroup: &str,
) -> Result<Vec<Table>, ApiError> {
    let mut items = Vec::new();
    let mut token = None;
    loop {
        let page = api
            .list_athena_tables(catalog, database, workgroup, token.as_deref())
            .await?;
        items.extend(page.items);
        if page.next_token.as_deref().is_none_or(str::is_empty) {
            return Ok(items);
        }
        token = page.next_token;
    }
}

fn map_error(
    error: ApiError,
    request: &Request<'_>,
    connection: &Connection,
    database: Option<&str>,
    table: Option<&str>,
) -> MetadataError {
    let operation = request.operation();
    match error {
        ApiError::ExpiredCredentials => MetadataError::CredentialsExpired { operation },
        ApiError::AccessDenied => MetadataError::AccessDenied {
            operation,
            catalog: connection.catalog.clone(),
        },
        ApiError::NotFound if operation == Operation::Describe => MetadataError::TableNotFound {
            catalog: connection.catalog.clone(),
            database: database.unwrap_or_default().to_owned(),
            table: table.unwrap_or_default().to_owned(),
        },
        ApiError::NotFound | ApiError::Unavailable => MetadataError::ProviderFailed {
            operation,
            catalog: connection.catalog.clone(),
        },
    }
}

fn is_glue_catalog(catalog: &str) -> bool {
    catalog == "AwsDataCatalog"
}

fn spec(name: &str, data_type: &str) -> ColumnSpec {
    ColumnSpec {
        name: name.to_owned(),
        data_type: data_type.to_owned(),
    }
}

fn catalog_columns() -> Vec<ColumnSpec> {
    vec![
        spec("catalog", "varchar"),
        spec("type", "varchar"),
        spec("status", "varchar"),
    ]
}

fn database_columns() -> Vec<ColumnSpec> {
    vec![
        spec("catalog", "varchar"),
        spec("database", "varchar"),
        spec("description", "varchar"),
        spec("location", "varchar"),
    ]
}

fn table_columns() -> Vec<ColumnSpec> {
    vec![
        spec("catalog", "varchar"),
        spec("database", "varchar"),
        spec("table", "varchar"),
        spec("table_type", "varchar"),
        spec("is_view", "boolean"),
        spec("is_iceberg", "boolean"),
        spec("location", "varchar"),
    ]
}

fn describe_columns() -> Vec<ColumnSpec> {
    vec![
        spec("catalog", "varchar"),
        spec("database", "varchar"),
        spec("table", "varchar"),
        spec("table_type", "varchar"),
        spec("is_view", "boolean"),
        spec("is_iceberg", "boolean"),
        spec("location", "varchar"),
        spec("column", "varchar"),
        spec("ordinal", "integer"),
        spec("type", "varchar"),
        spec("partition_key", "boolean"),
        spec("comment", "varchar"),
        spec("parameters", "varchar"),
    ]
}

fn catalog_rows(items: &[Catalog]) -> Vec<Vec<Option<String>>> {
    items
        .iter()
        .map(|item| {
            vec![
                Some(item.name.clone()),
                item.kind.clone(),
                item.status.clone(),
            ]
        })
        .collect()
}

fn database_rows(catalog: &str, items: &[Database]) -> Vec<Vec<Option<String>>> {
    items
        .iter()
        .map(|item| {
            vec![
                Some(catalog.to_owned()),
                Some(item.name.clone()),
                item.description.clone(),
                item.location.clone(),
            ]
        })
        .collect()
}

fn table_rows(catalog: &str, database: &str, items: &[Table]) -> Vec<Vec<Option<String>>> {
    items
        .iter()
        .map(|item| {
            vec![
                Some(catalog.to_owned()),
                Some(database.to_owned()),
                Some(item.name.clone()),
                item.kind.clone(),
                Some(item.is_view().to_string()),
                Some(item.is_iceberg().to_string()),
                item.location.clone(),
            ]
        })
        .collect()
}

fn describe_rows(catalog: &str, database: &str, table: &Table) -> Vec<Vec<Option<String>>> {
    let parameters = serde_json::to_string(&table.parameters).expect("string maps serialize");
    let mut rows = Vec::new();
    for (index, column) in table.columns.iter().enumerate() {
        rows.push(describe_row(
            catalog,
            database,
            table,
            column,
            index + 1,
            false,
            &parameters,
        ));
    }
    let offset = table.columns.len();
    for (index, column) in table.partition_keys.iter().enumerate() {
        rows.push(describe_row(
            catalog,
            database,
            table,
            column,
            offset + index + 1,
            true,
            &parameters,
        ));
    }
    if rows.is_empty() {
        rows.push(vec![
            Some(catalog.to_owned()),
            Some(database.to_owned()),
            Some(table.name.clone()),
            table.kind.clone(),
            Some(table.is_view().to_string()),
            Some(table.is_iceberg().to_string()),
            table.location.clone(),
            None,
            None,
            None,
            None,
            None,
            Some(parameters),
        ]);
    }
    rows
}

fn describe_row(
    catalog: &str,
    database: &str,
    table: &Table,
    column: &api::MetadataColumn,
    ordinal: usize,
    partition_key: bool,
    parameters: &str,
) -> Vec<Option<String>> {
    vec![
        Some(catalog.to_owned()),
        Some(database.to_owned()),
        Some(table.name.clone()),
        table.kind.clone(),
        Some(table.is_view().to_string()),
        Some(table.is_iceberg().to_string()),
        table.location.clone(),
        Some(column.name.clone()),
        Some(ordinal.to_string()),
        column.data_type.clone(),
        Some(partition_key.to_string()),
        column.comment.clone(),
        Some(parameters.to_owned()),
    ]
}

#[allow(clippy::too_many_arguments)]
fn write_operations(
    writer: &mut dyn io::Write,
    connection: &Connection,
    session_name: &str,
    operation: Operation,
    database: Option<&str>,
    rows: usize,
    output_path: Option<&Path>,
    quiet: bool,
) -> io::Result<()> {
    if quiet {
        return Ok(());
    }
    writeln!(
        writer,
        "Connection: {}",
        crate::diagnostics::terminal_text(&connection.name)
    )?;
    writeln!(
        writer,
        "Session: {}",
        crate::diagnostics::terminal_text(session_name)
    )?;
    writeln!(writer, "Operation: {operation}")?;
    writeln!(
        writer,
        "Catalog: {}",
        crate::diagnostics::terminal_text(&connection.catalog)
    )?;
    if let Some(database) = database {
        writeln!(
            writer,
            "Database: {}",
            crate::diagnostics::terminal_text(database)
        )?;
    }
    writeln!(writer, "Rows: {rows}")?;
    if let Some(path) = output_path {
        writeln!(
            writer,
            "Output: {}",
            crate::diagnostics::terminal_text(&path.display().to_string())
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::*;
    use crate::config::Policy;
    use crate::metadata::api::{ApiFuture, MetadataColumn, Page};

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum Call {
        Catalogs(Option<String>),
        GlueDatabases,
        AthenaDatabases,
        GlueTables,
        AthenaTables,
        GlueDescribe,
        AthenaDescribe,
    }

    #[derive(Default)]
    struct MockMetadata {
        calls: Mutex<Vec<Call>>,
        malformed_catalog: AtomicBool,
        describe_error: Mutex<Option<ApiError>>,
    }

    impl MockMetadata {
        fn record(&self, call: Call) {
            self.calls.lock().expect("calls").push(call);
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.lock().expect("calls").clone()
        }
    }

    impl MetadataApi for MockMetadata {
        fn list_catalogs<'a>(
            &'a self,
            _workgroup: &'a str,
            next_token: Option<&'a str>,
        ) -> ApiFuture<'a, Page<Catalog>> {
            self.record(Call::Catalogs(next_token.map(str::to_owned)));
            Box::pin(async move {
                let (name, next_token) = if next_token.is_none() {
                    ("first", Some("page-2".to_owned()))
                } else {
                    ("second", None)
                };
                Ok(Page {
                    items: vec![Catalog {
                        name: if self.malformed_catalog.load(Ordering::SeqCst) {
                            String::new()
                        } else {
                            name.to_owned()
                        },
                        kind: None,
                        status: None,
                    }],
                    next_token,
                })
            })
        }

        fn list_glue_databases<'a>(
            &'a self,
            _account: &'a str,
            _next_token: Option<&'a str>,
        ) -> ApiFuture<'a, Page<Database>> {
            self.record(Call::GlueDatabases);
            Box::pin(async { Ok(empty_page()) })
        }

        fn list_athena_databases<'a>(
            &'a self,
            _catalog: &'a str,
            _workgroup: &'a str,
            _next_token: Option<&'a str>,
        ) -> ApiFuture<'a, Page<Database>> {
            self.record(Call::AthenaDatabases);
            Box::pin(async { Ok(empty_page()) })
        }

        fn list_glue_tables<'a>(
            &'a self,
            _account: &'a str,
            _database: &'a str,
            _next_token: Option<&'a str>,
        ) -> ApiFuture<'a, Page<Table>> {
            self.record(Call::GlueTables);
            Box::pin(async { Ok(empty_page()) })
        }

        fn list_athena_tables<'a>(
            &'a self,
            _catalog: &'a str,
            _database: &'a str,
            _workgroup: &'a str,
            _next_token: Option<&'a str>,
        ) -> ApiFuture<'a, Page<Table>> {
            self.record(Call::AthenaTables);
            Box::pin(async { Ok(empty_page()) })
        }

        fn get_glue_table<'a>(
            &'a self,
            _account: &'a str,
            _database: &'a str,
            table: &'a str,
        ) -> ApiFuture<'a, Table> {
            self.record(Call::GlueDescribe);
            Box::pin(async move {
                match *self.describe_error.lock().expect("describe error") {
                    Some(error) => Err(error),
                    None => Ok(empty_table(table)),
                }
            })
        }

        fn get_athena_table<'a>(
            &'a self,
            _catalog: &'a str,
            _database: &'a str,
            table: &'a str,
            _workgroup: &'a str,
        ) -> ApiFuture<'a, Table> {
            self.record(Call::AthenaDescribe);
            Box::pin(async move {
                match *self.describe_error.lock().expect("describe error") {
                    Some(error) => Err(error),
                    None => Ok(empty_table(table)),
                }
            })
        }
    }

    fn empty_page<T>() -> Page<T> {
        Page {
            items: Vec::new(),
            next_token: None,
        }
    }

    fn empty_table(name: &str) -> Table {
        Table {
            name: name.to_owned(),
            kind: None,
            location: None,
            parameters: std::collections::BTreeMap::default(),
            columns: Vec::new(),
            partition_keys: Vec::new(),
            has_view_text: false,
            has_iceberg_metadata: false,
        }
    }

    fn connection(catalog: &str) -> Connection {
        Connection {
            name: "dev".into(),
            account: "123456789012".into(),
            region: "eu-west-1".into(),
            role_arn: "arn:aws:iam::123456789012:role/test".into(),
            workgroup: "primary".into(),
            catalog: catalog.into(),
            database: "analytics".into(),
            output_location: None,
            policy: Policy::ReadOnly,
            query_timeout: Duration::from_secs(60),
        }
    }

    #[test]
    fn parses_default_and_qualified_objects() {
        assert_eq!(
            parse_object("orders", "analytics"),
            Ok(ObjectName {
                database: "analytics".into(),
                table: "orders".into(),
            })
        );
        assert_eq!(
            parse_object("archive.orders", "analytics"),
            Ok(ObjectName {
                database: "archive".into(),
                table: "orders".into(),
            })
        );
        assert!(parse_object("a.b.c", "analytics").is_err());
        assert!(parse_object(".orders", "analytics").is_err());
    }

    #[test]
    fn identifies_views_and_iceberg_tables() {
        let mut parameters = std::collections::BTreeMap::new();
        parameters.insert("table_type".into(), "ICEBERG".into());
        let table = Table {
            name: "orders".into(),
            kind: Some("VIRTUAL_VIEW".into()),
            location: None,
            parameters,
            columns: Vec::new(),
            partition_keys: Vec::new(),
            has_view_text: false,
            has_iceberg_metadata: false,
        };
        assert!(table.is_view());
        assert!(table.is_iceberg());
    }

    #[test]
    fn describe_repeats_table_details_and_marks_partition_keys() {
        let table = Table {
            name: "orders".into(),
            kind: Some("EXTERNAL_TABLE".into()),
            location: Some("s3://lake/orders".into()),
            parameters: [("table_type".into(), "ICEBERG".into())].into(),
            columns: vec![MetadataColumn {
                name: "id".into(),
                data_type: Some("bigint".into()),
                comment: None,
            }],
            partition_keys: vec![MetadataColumn {
                name: "day".into(),
                data_type: Some("date".into()),
                comment: Some("UTC day".into()),
            }],
            has_view_text: false,
            has_iceberg_metadata: false,
        };
        let rows = describe_rows("AwsDataCatalog", "analytics", &table);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][7].as_deref(), Some("id"));
        assert_eq!(rows[0][10].as_deref(), Some("false"));
        assert_eq!(rows[1][7].as_deref(), Some("day"));
        assert_eq!(rows[1][10].as_deref(), Some("true"));
        assert_eq!(rows[1][11].as_deref(), Some("UTC day"));
        assert_eq!(rows[1][5].as_deref(), Some("true"));
    }

    #[test]
    fn metadata_field_names_are_stable() {
        let names = |columns: Vec<ColumnSpec>| {
            columns
                .into_iter()
                .map(|column| column.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(catalog_columns()), ["catalog", "type", "status"]);
        assert_eq!(
            names(database_columns()),
            ["catalog", "database", "description", "location"]
        );
        assert_eq!(
            names(table_columns()),
            [
                "catalog",
                "database",
                "table",
                "table_type",
                "is_view",
                "is_iceberg",
                "location",
            ]
        );
        assert_eq!(
            names(describe_columns()),
            [
                "catalog",
                "database",
                "table",
                "table_type",
                "is_view",
                "is_iceberg",
                "location",
                "column",
                "ordinal",
                "type",
                "partition_key",
                "comment",
                "parameters",
            ]
        );
    }

    #[tokio::test]
    async fn catalog_pagination_passes_each_token_and_keeps_every_page() {
        let api = MockMetadata::default();
        let catalogs = all_catalogs(&api, "primary").await.expect("catalogs");
        assert_eq!(
            catalogs
                .iter()
                .map(|catalog| catalog.name.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert_eq!(
            api.calls(),
            [Call::Catalogs(None), Call::Catalogs(Some("page-2".into()))]
        );
    }

    #[tokio::test]
    async fn glue_catalog_uses_glue_and_federated_catalog_uses_athena() {
        let api = MockMetadata::default();
        let glue = connection("AwsDataCatalog");
        discover(&api, &Request::Databases, &Context { connection: &glue })
            .await
            .expect("Glue databases");
        discover(
            &api,
            &Request::Describe {
                database: "analytics",
                table: "orders",
            },
            &Context { connection: &glue },
        )
        .await
        .expect("Glue describe");

        let federated = connection("warehouse");
        discover(
            &api,
            &Request::Tables {
                database: "analytics",
            },
            &Context {
                connection: &federated,
            },
        )
        .await
        .expect("Athena tables");
        discover(
            &api,
            &Request::Describe {
                database: "analytics",
                table: "orders",
            },
            &Context {
                connection: &federated,
            },
        )
        .await
        .expect("Athena describe");

        assert_eq!(
            api.calls(),
            [
                Call::GlueDatabases,
                Call::GlueDescribe,
                Call::AthenaTables,
                Call::AthenaDescribe,
            ]
        );
    }

    #[tokio::test]
    async fn malformed_catalogs_and_missing_tables_have_stable_errors() {
        let malformed = MockMetadata::default();
        malformed.malformed_catalog.store(true, Ordering::SeqCst);
        let connection = connection("AwsDataCatalog");
        let error = discover(
            &malformed,
            &Request::Catalogs,
            &Context {
                connection: &connection,
            },
        )
        .await
        .expect_err("catalog name is required");
        assert!(matches!(
            error,
            MetadataError::MalformedResponse {
                operation: Operation::Catalogs
            }
        ));
        assert_eq!(error.exit_code(), 4);

        let missing = MockMetadata::default();
        *missing.describe_error.lock().expect("describe error") = Some(ApiError::NotFound);
        let error = discover(
            &missing,
            &Request::Describe {
                database: "analytics",
                table: "orders",
            },
            &Context {
                connection: &connection,
            },
        )
        .await
        .expect_err("missing table");
        assert!(matches!(error, MetadataError::TableNotFound { .. }));
        assert_eq!(error.exit_code(), 4);
        assert!(error.to_string().contains("analytics"));
        assert!(error.to_string().contains("orders"));
    }

    #[test]
    fn absent_optional_table_fields_still_produce_one_description_row() {
        let rows = describe_rows("AwsDataCatalog", "analytics", &empty_table("orders"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][2].as_deref(), Some("orders"));
        assert_eq!(rows[0][3], None);
        assert_eq!(rows[0][4].as_deref(), Some("false"));
        assert_eq!(rows[0][5].as_deref(), Some("false"));
        assert_eq!(rows[0][7], None);
        assert_eq!(rows[0][12].as_deref(), Some("{}"));
    }

    #[test]
    fn metadata_operations_escape_control_characters() {
        let mut connection = connection("AwsDataCatalog");
        connection.name = "dev\nspoofed".into();
        let mut output = Vec::new();
        write_operations(
            &mut output,
            &connection,
            "session\u{1b}[31m",
            Operation::Tables,
            Some("analytics\rname"),
            0,
            None,
            false,
        )
        .expect("operations");
        let output = String::from_utf8(output).expect("UTF-8");
        assert!(!output.contains('\u{1b}'));
        assert!(!output.contains('\r'));
        assert_eq!(output.lines().count(), 6);
        assert!(output.contains("dev\\u{a}spoofed"));
        assert!(output.contains("session\\u{1b}[31m"));
        assert!(output.contains("analytics\\u{d}name"));
    }
}
