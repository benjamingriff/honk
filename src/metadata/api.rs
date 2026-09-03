use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

pub(super) type ApiFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ApiError>> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ApiError {
    ExpiredCredentials,
    AccessDenied,
    NotFound,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Page<T> {
    pub(super) items: Vec<T>,
    pub(super) next_token: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Catalog {
    pub(super) name: String,
    pub(super) kind: Option<String>,
    pub(super) status: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Database {
    pub(super) name: String,
    pub(super) description: Option<String>,
    pub(super) location: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct MetadataColumn {
    pub(super) name: String,
    pub(super) data_type: Option<String>,
    pub(super) comment: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Table {
    pub(super) name: String,
    pub(super) kind: Option<String>,
    pub(super) location: Option<String>,
    pub(super) parameters: BTreeMap<String, String>,
    pub(super) columns: Vec<MetadataColumn>,
    pub(super) partition_keys: Vec<MetadataColumn>,
    pub(super) has_view_text: bool,
    pub(super) has_iceberg_metadata: bool,
}

impl Table {
    pub(super) fn is_view(&self) -> bool {
        self.has_view_text
            || self
                .kind
                .as_deref()
                .is_some_and(|value| value.to_ascii_uppercase().contains("VIEW"))
    }

    pub(super) fn is_iceberg(&self) -> bool {
        self.has_iceberg_metadata
            || self.parameters.iter().any(|(key, value)| {
                (key.eq_ignore_ascii_case("table_type")
                    || key.eq_ignore_ascii_case("classification"))
                    && value.eq_ignore_ascii_case("iceberg")
            })
            || self
                .parameters
                .keys()
                .any(|key| key.eq_ignore_ascii_case("metadata_location"))
    }
}

/// The discovery boundary deliberately contains no query-execution operation.
pub(super) trait MetadataApi: Send + Sync {
    fn list_catalogs<'a>(
        &'a self,
        workgroup: &'a str,
        next_token: Option<&'a str>,
    ) -> ApiFuture<'a, Page<Catalog>>;

    fn list_glue_databases<'a>(
        &'a self,
        account: &'a str,
        next_token: Option<&'a str>,
    ) -> ApiFuture<'a, Page<Database>>;

    fn list_athena_databases<'a>(
        &'a self,
        catalog: &'a str,
        workgroup: &'a str,
        next_token: Option<&'a str>,
    ) -> ApiFuture<'a, Page<Database>>;

    fn list_glue_tables<'a>(
        &'a self,
        account: &'a str,
        database: &'a str,
        next_token: Option<&'a str>,
    ) -> ApiFuture<'a, Page<Table>>;

    fn list_athena_tables<'a>(
        &'a self,
        catalog: &'a str,
        database: &'a str,
        workgroup: &'a str,
        next_token: Option<&'a str>,
    ) -> ApiFuture<'a, Page<Table>>;

    fn get_glue_table<'a>(
        &'a self,
        account: &'a str,
        database: &'a str,
        table: &'a str,
    ) -> ApiFuture<'a, Table>;

    fn get_athena_table<'a>(
        &'a self,
        catalog: &'a str,
        database: &'a str,
        table: &'a str,
        workgroup: &'a str,
    ) -> ApiFuture<'a, Table>;
}
