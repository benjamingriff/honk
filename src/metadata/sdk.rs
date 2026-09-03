use std::collections::BTreeMap;

use aws_config::BehaviorVersion;
use aws_credential_types::provider::SharedCredentialsProvider;
use aws_sdk_athena::config::Region;
use aws_sdk_athena::error::ProvideErrorMetadata as _;

use super::api::{
    ApiError, ApiFuture, Catalog, Database, MetadataApi, MetadataColumn, Page, Table,
};

#[derive(Clone, Debug)]
pub(super) struct SdkMetadata {
    athena: aws_sdk_athena::Client,
    glue: aws_sdk_glue::Client,
}

impl SdkMetadata {
    pub(super) async fn new(provider: SharedCredentialsProvider, region: &str) -> Self {
        let sdk_config = aws_config::defaults(BehaviorVersion::latest())
            .empty_test_environment()
            .region(Region::new(region.to_owned()))
            .credentials_provider(provider)
            .load()
            .await;
        Self {
            athena: aws_sdk_athena::Client::new(&sdk_config),
            glue: aws_sdk_glue::Client::new(&sdk_config),
        }
    }
}

impl MetadataApi for SdkMetadata {
    fn list_catalogs<'a>(
        &'a self,
        workgroup: &'a str,
        next_token: Option<&'a str>,
    ) -> ApiFuture<'a, Page<Catalog>> {
        Box::pin(async move {
            let response = self
                .athena
                .list_data_catalogs()
                .work_group(workgroup)
                .max_results(50)
                .set_next_token(next_token.map(str::to_owned))
                .send()
                .await
                .map_err(|error| classify(error.as_service_error().and_then(|e| e.code())))?;
            Ok(Page {
                items: response
                    .data_catalogs_summary()
                    .iter()
                    .map(|catalog| Catalog {
                        name: catalog.catalog_name().unwrap_or_default().to_owned(),
                        kind: catalog.r#type().map(|value| value.as_str().to_owned()),
                        status: catalog.status().map(|value| value.as_str().to_owned()),
                    })
                    .collect(),
                next_token: response.next_token().map(str::to_owned),
            })
        })
    }

    fn list_glue_databases<'a>(
        &'a self,
        account: &'a str,
        next_token: Option<&'a str>,
    ) -> ApiFuture<'a, Page<Database>> {
        Box::pin(async move {
            let response = self
                .glue
                .get_databases()
                .catalog_id(account)
                .max_results(100)
                .set_next_token(next_token.map(str::to_owned))
                .send()
                .await
                .map_err(|error| classify(error.as_service_error().and_then(|e| e.code())))?;
            Ok(Page {
                items: response
                    .database_list()
                    .iter()
                    .map(|database| Database {
                        name: database.name().to_owned(),
                        description: database.description().map(str::to_owned),
                        location: database.location_uri().map(str::to_owned),
                    })
                    .collect(),
                next_token: response.next_token().map(str::to_owned),
            })
        })
    }

    fn list_athena_databases<'a>(
        &'a self,
        catalog: &'a str,
        workgroup: &'a str,
        next_token: Option<&'a str>,
    ) -> ApiFuture<'a, Page<Database>> {
        Box::pin(async move {
            let response = self
                .athena
                .list_databases()
                .catalog_name(catalog)
                .work_group(workgroup)
                .max_results(50)
                .set_next_token(next_token.map(str::to_owned))
                .send()
                .await
                .map_err(|error| classify(error.as_service_error().and_then(|e| e.code())))?;
            Ok(Page {
                items: response
                    .database_list()
                    .iter()
                    .map(|database| Database {
                        name: database.name().to_owned(),
                        description: database.description().map(str::to_owned),
                        location: None,
                    })
                    .collect(),
                next_token: response.next_token().map(str::to_owned),
            })
        })
    }

    fn list_glue_tables<'a>(
        &'a self,
        account: &'a str,
        database: &'a str,
        next_token: Option<&'a str>,
    ) -> ApiFuture<'a, Page<Table>> {
        Box::pin(async move {
            let response = self
                .glue
                .get_tables()
                .catalog_id(account)
                .database_name(database)
                .max_results(100)
                .set_next_token(next_token.map(str::to_owned))
                .send()
                .await
                .map_err(|error| classify(error.as_service_error().and_then(|e| e.code())))?;
            Ok(Page {
                items: response.table_list().iter().map(glue_table).collect(),
                next_token: response.next_token().map(str::to_owned),
            })
        })
    }

    fn list_athena_tables<'a>(
        &'a self,
        catalog: &'a str,
        database: &'a str,
        workgroup: &'a str,
        next_token: Option<&'a str>,
    ) -> ApiFuture<'a, Page<Table>> {
        Box::pin(async move {
            let response = self
                .athena
                .list_table_metadata()
                .catalog_name(catalog)
                .database_name(database)
                .work_group(workgroup)
                .max_results(50)
                .set_next_token(next_token.map(str::to_owned))
                .send()
                .await
                .map_err(|error| classify(error.as_service_error().and_then(|e| e.code())))?;
            Ok(Page {
                items: response
                    .table_metadata_list()
                    .iter()
                    .map(athena_table)
                    .collect(),
                next_token: response.next_token().map(str::to_owned),
            })
        })
    }

    fn get_glue_table<'a>(
        &'a self,
        account: &'a str,
        database: &'a str,
        table: &'a str,
    ) -> ApiFuture<'a, Table> {
        Box::pin(async move {
            let response = self
                .glue
                .get_table()
                .catalog_id(account)
                .database_name(database)
                .name(table)
                .send()
                .await
                .map_err(|error| classify(error.as_service_error().and_then(|e| e.code())))?;
            response
                .table()
                .map(glue_table)
                .ok_or(ApiError::Unavailable)
        })
    }

    fn get_athena_table<'a>(
        &'a self,
        catalog: &'a str,
        database: &'a str,
        table: &'a str,
        workgroup: &'a str,
    ) -> ApiFuture<'a, Table> {
        Box::pin(async move {
            let response = self
                .athena
                .get_table_metadata()
                .catalog_name(catalog)
                .database_name(database)
                .table_name(table)
                .work_group(workgroup)
                .send()
                .await
                .map_err(|error| classify(error.as_service_error().and_then(|e| e.code())))?;
            response
                .table_metadata()
                .map(athena_table)
                .ok_or(ApiError::Unavailable)
        })
    }
}

fn glue_table(table: &aws_sdk_glue::types::Table) -> Table {
    let descriptor = table.storage_descriptor();
    Table {
        name: table.name().to_owned(),
        kind: table.table_type().map(str::to_owned),
        location: descriptor
            .and_then(|value| value.location())
            .map(str::to_owned),
        parameters: ordered(table.parameters()),
        columns: descriptor
            .map(aws_sdk_glue::types::StorageDescriptor::columns)
            .unwrap_or_default()
            .iter()
            .map(|column| MetadataColumn {
                name: column.name().to_owned(),
                data_type: column.r#type().map(str::to_owned),
                comment: column.comment().map(str::to_owned),
            })
            .collect(),
        partition_keys: table
            .partition_keys()
            .iter()
            .map(|column| MetadataColumn {
                name: column.name().to_owned(),
                data_type: column.r#type().map(str::to_owned),
                comment: column.comment().map(str::to_owned),
            })
            .collect(),
        has_view_text: table.view_original_text().is_some() || table.view_definition().is_some(),
        has_iceberg_metadata: table.iceberg_table_metadata().is_some(),
    }
}

fn athena_table(table: &aws_sdk_athena::types::TableMetadata) -> Table {
    let parameters = ordered(table.parameters());
    Table {
        name: table.name().to_owned(),
        kind: table.table_type().map(str::to_owned),
        location: parameters.get("location").cloned(),
        parameters,
        columns: table.columns().iter().map(athena_column).collect(),
        partition_keys: table.partition_keys().iter().map(athena_column).collect(),
        has_view_text: false,
        has_iceberg_metadata: false,
    }
}

fn athena_column(column: &aws_sdk_athena::types::Column) -> MetadataColumn {
    MetadataColumn {
        name: column.name().to_owned(),
        data_type: column.r#type().map(str::to_owned),
        comment: column.comment().map(str::to_owned),
    }
}

fn ordered(map: Option<&std::collections::HashMap<String, String>>) -> BTreeMap<String, String> {
    map.into_iter()
        .flat_map(std::collections::HashMap::iter)
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn classify(code: Option<&str>) -> ApiError {
    if crate::auth::is_expired_aws_error_code(code) {
        ApiError::ExpiredCredentials
    } else {
        match code {
            Some("AccessDenied" | "AccessDeniedException" | "UnauthorizedException") => {
                ApiError::AccessDenied
            }
            Some("EntityNotFoundException" | "ResourceNotFoundException") => ApiError::NotFound,
            _ => ApiError::Unavailable,
        }
    }
}
