use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::time::Duration;

use crate::error::AppError;

const CONNECTION_FIELDS: [&str; 9] = [
    "account",
    "region",
    "role_arn",
    "workgroup",
    "catalog",
    "database",
    "output_location",
    "policy",
    "query_timeout",
];

#[derive(Debug)]
pub struct Config {
    connections: BTreeMap<String, Connection>,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct Connection {
    pub name: String,
    pub account: String,
    pub region: String,
    pub role_arn: String,
    pub workgroup: String,
    pub catalog: String,
    pub database: String,
    pub output_location: Option<String>,
    pub policy: Policy,
    pub query_timeout: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Policy {
    ReadOnly,
}

impl Config {
    pub(crate) fn load(path: &Path) -> Result<Self, AppError> {
        let source = fs::read_to_string(path).map_err(|source| AppError::ReadConfig {
            path: path.to_path_buf(),
            source,
        })?;

        Self::parse(&source).map_err(|issues| AppError::InvalidConfig {
            path: path.to_path_buf(),
            details: render_issues(&issues),
        })
    }

    fn parse(source: &str) -> Result<Self, Vec<String>> {
        let mut issues = Vec::new();
        let root = source
            .parse::<toml::Table>()
            .map_err(|error| vec![render_toml_parse_error(source, &error)])?;

        for field in root.keys() {
            if field != "connections" {
                issues.push(format!("configuration has unknown field {field:?}"));
            }
        }

        let Some(raw_connections) = root.get("connections") else {
            issues.push("configuration is missing required field \"connections\"".to_owned());
            return Err(issues);
        };
        let Some(raw_connections) = raw_connections.as_table() else {
            issues.push("field \"connections\" must be a TOML table".to_owned());
            return Err(issues);
        };
        if raw_connections.is_empty() {
            issues.push("connections must contain at least one connection".to_owned());
            return Err(issues);
        }

        let mut connections = BTreeMap::new();

        for (name, value) in raw_connections {
            if let Some(connection) = parse_connection(name, value, &mut issues) {
                connections.insert(name.clone(), connection);
            }
        }

        if issues.is_empty() {
            Ok(Self { connections })
        } else {
            Err(issues)
        }
    }

    pub(crate) fn connection(&self, name: &str) -> Result<&Connection, AppError> {
        self.connections
            .get(name)
            .ok_or_else(|| AppError::UnknownConnection {
                name: name.to_owned(),
                available: self
                    .connections
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
            })
    }

    pub(crate) fn len(&self) -> usize {
        self.connections.len()
    }

    pub(crate) fn render_connections(&self) -> String {
        let mut output = String::new();
        for connection in self.connections.values() {
            let _ = writeln!(output, "{}", connection.name);
            let _ = writeln!(output, "  account: {}", connection.account);
            let _ = writeln!(output, "  region: {}", connection.region);
            let _ = writeln!(output, "  role: {}", connection.role_arn);
            let _ = writeln!(output, "  workgroup: {}", connection.workgroup);
            let _ = writeln!(output, "  catalog: {}", connection.catalog);
            let _ = writeln!(output, "  database: {}", connection.database);
            let _ = writeln!(output, "  policy: read_only");
            let _ = writeln!(
                output,
                "  query timeout: {}",
                humantime::format_duration(connection.query_timeout)
            );
            if let Some(location) = &connection.output_location {
                let _ = writeln!(output, "  output location: {location}");
            }
        }
        output
    }
}

impl Connection {
    pub(crate) fn expected_role_name(&self) -> Option<&str> {
        self.role_arn
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
    }
}

fn parse_connection(
    name: &str,
    value: &toml::Value,
    issues: &mut Vec<String>,
) -> Option<Connection> {
    if name.trim().is_empty() {
        issues.push("connection names cannot be empty or whitespace".to_owned());
    }

    let Some(table) = value.as_table() else {
        issues.push(format!("connection {name:?} must be a TOML table"));
        return None;
    };

    let allowed = CONNECTION_FIELDS.into_iter().collect::<BTreeSet<_>>();
    for field in table.keys() {
        if !allowed.contains(field.as_str()) {
            issues.push(format!("connection {name:?} has unknown field {field:?}"));
        }
    }

    let account = required_string(table, name, "account", issues);
    let region = required_string(table, name, "region", issues);
    let role_arn = required_string(table, name, "role_arn", issues);
    let workgroup = required_string(table, name, "workgroup", issues);
    let catalog = required_string(table, name, "catalog", issues);
    let database = required_string(table, name, "database", issues);
    let policy = required_string(table, name, "policy", issues);
    let query_timeout = required_string(table, name, "query_timeout", issues);
    let output_location = optional_string(table, name, "output_location", issues);

    if let Some(value) = &account
        && (value.len() != 12 || !value.bytes().all(|byte| byte.is_ascii_digit()))
    {
        issues.push(format!(
            "connection {name:?} field \"account\" must contain exactly 12 digits"
        ));
    }

    if let Some(value) = &region
        && !valid_region(value)
    {
        issues.push(format!(
            "connection {name:?} field \"region\" is not a valid AWS region name"
        ));
    }

    if let (Some(account), Some(role_arn)) = (&account, &role_arn) {
        match role_arn_account(role_arn) {
            Some(role_account) if role_account == account => {}
            Some(role_account) => issues.push(format!(
                "connection {name:?} role ARN account {role_account:?} does not match configured account {account:?}"
            )),
            None => issues.push(format!(
                "connection {name:?} field \"role_arn\" is not a valid IAM role ARN"
            )),
        }
    }

    if let Some(value) = &output_location
        && !valid_s3_uri(value)
    {
        issues.push(format!(
            "connection {name:?} field \"output_location\" must be an s3:// URI with a bucket name"
        ));
    }

    let parsed_policy = match policy.as_deref() {
        Some("read_only") => Some(Policy::ReadOnly),
        Some(value) => {
            issues.push(format!(
                "connection {name:?} field \"policy\" must be \"read_only\", not {value:?}"
            ));
            None
        }
        None => None,
    };

    let parsed_timeout =
        query_timeout
            .as_deref()
            .and_then(|value| match humantime::parse_duration(value) {
                Ok(duration) if !duration.is_zero() => Some(duration),
                Ok(_) => {
                    issues.push(format!(
                        "connection {name:?} field \"query_timeout\" must be greater than zero"
                    ));
                    None
                }
                Err(error) => {
                    issues.push(format!(
                        "connection {name:?} field \"query_timeout\" is invalid: {error}"
                    ));
                    None
                }
            });

    Some(Connection {
        name: name.to_owned(),
        account: account?,
        region: region?,
        role_arn: role_arn?,
        workgroup: workgroup?,
        catalog: catalog?,
        database: database?,
        output_location,
        policy: parsed_policy?,
        query_timeout: parsed_timeout?,
    })
}

fn required_string(
    table: &toml::Table,
    connection: &str,
    field: &str,
    issues: &mut Vec<String>,
) -> Option<String> {
    match table.get(field) {
        Some(toml::Value::String(value)) if !value.trim().is_empty() => Some(value.clone()),
        Some(toml::Value::String(_)) => {
            issues.push(format!(
                "connection {connection:?} field {field:?} cannot be empty or whitespace"
            ));
            None
        }
        Some(_) => {
            issues.push(format!(
                "connection {connection:?} field {field:?} must be a string"
            ));
            None
        }
        None => {
            issues.push(format!(
                "connection {connection:?} is missing required field {field:?}"
            ));
            None
        }
    }
}

fn optional_string(
    table: &toml::Table,
    connection: &str,
    field: &str,
    issues: &mut Vec<String>,
) -> Option<String> {
    match table.get(field) {
        Some(toml::Value::String(value)) if !value.trim().is_empty() => Some(value.clone()),
        Some(toml::Value::String(_)) => {
            issues.push(format!(
                "connection {connection:?} field {field:?} cannot be empty or whitespace"
            ));
            None
        }
        Some(_) => {
            issues.push(format!(
                "connection {connection:?} field {field:?} must be a string"
            ));
            None
        }
        None => None,
    }
}

fn valid_region(value: &str) -> bool {
    let parts = value.split('-').collect::<Vec<_>>();
    parts.len() >= 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
        && parts
            .last()
            .is_some_and(|part| part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn role_arn_account(value: &str) -> Option<&str> {
    let parts = value.splitn(6, ':').collect::<Vec<_>>();
    if parts.len() != 6
        || parts[0] != "arn"
        || parts[1].is_empty()
        || parts[2] != "iam"
        || !parts[3].is_empty()
        || parts[4].len() != 12
        || !parts[4].bytes().all(|byte| byte.is_ascii_digit())
        || !parts[5].starts_with("role/")
        || parts[5] == "role/"
    {
        None
    } else {
        Some(parts[4])
    }
}

fn valid_s3_uri(value: &str) -> bool {
    value
        .strip_prefix("s3://")
        .and_then(|rest| rest.split('/').next())
        .is_some_and(|bucket| !bucket.is_empty() && !bucket.chars().any(char::is_whitespace))
}

fn render_issues(issues: &[String]) -> String {
    issues
        .iter()
        .map(|issue| format!("  - {issue}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_toml_parse_error(source: &str, error: &toml::de::Error) -> String {
    let Some(span) = error.span() else {
        return format!("TOML could not be parsed: {}", error.message());
    };
    let offset = span.start.min(source.len());
    let before = &source[..offset];
    let line = before.split('\n').count();
    let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
    let column = source[line_start..offset].chars().count() + 1;
    format!(
        "TOML could not be parsed at line {line}, column {column}: {}",
        error.message()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
[connections.dev]
account = "111111111111"
region = "eu-west-1"
role_arn = "arn:aws:iam::111111111111:role/honk-readonly"
workgroup = "analytics-dev"
catalog = "AwsDataCatalog"
database = "analytics_dev"
output_location = "s3://company-results/dev/"
policy = "read_only"
query_timeout = "30m"
"#;

    #[test]
    fn parses_a_valid_connection() {
        let config = Config::parse(VALID).expect("valid configuration");
        let connection = config.connection("dev").expect("dev connection");
        assert_eq!(connection.account, "111111111111");
        assert_eq!(connection.query_timeout, Duration::from_mins(30));
        assert_eq!(connection.policy, Policy::ReadOnly);
    }

    #[test]
    fn reports_multiple_connection_errors() {
        let invalid = r#"
[connections.dev]
account = "123"
region = "EU"
role_arn = "arn:aws:iam::999999999999:role/example"
workgroup = ""
catalog = "AwsDataCatalog"
database = "analytics"
output_location = "https://example.com/results"
policy = "write"
query_timeout = "never"
surprise = true
"#;

        let issues = Config::parse(invalid).expect_err("invalid configuration");
        assert_eq!(issues.len(), 8);
        assert!(issues.iter().any(|issue| issue.contains("unknown field")));
        assert!(
            issues
                .iter()
                .any(|issue| issue.contains("exactly 12 digits"))
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.contains("valid AWS region"))
        );
        assert!(issues.iter().any(|issue| issue.contains("does not match")));
        assert!(issues.iter().any(|issue| issue.contains("cannot be empty")));
        assert!(
            issues
                .iter()
                .any(|issue| issue.contains("must be an s3://"))
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.contains("must be \"read_only\""))
        );
        assert!(issues.iter().any(|issue| issue.contains("is invalid")));
    }

    #[test]
    fn rejects_unknown_root_fields() {
        let source = format!("secret = \"value\"\n{VALID}");
        let issues = Config::parse(&source).expect_err("unknown root field");
        assert!(issues[0].contains("unknown field"));
    }

    #[test]
    fn type_errors_do_not_echo_the_supplied_value() {
        let issues = Config::parse("connections = \"DO_NOT_PRINT_ME\"")
            .expect_err("connections must be a table");
        assert!(issues[0].contains("must be a TOML table"));
        assert!(!issues.join("\n").contains("DO_NOT_PRINT_ME"));
    }

    #[test]
    fn reports_every_missing_connection_field() {
        let issues = Config::parse("[connections.dev]").expect_err("missing fields");
        for field in [
            "account",
            "region",
            "role_arn",
            "workgroup",
            "catalog",
            "database",
            "policy",
            "query_timeout",
        ] {
            assert!(
                issues.iter().any(|issue| issue.contains(field)),
                "missing issue for {field}: {issues:?}"
            );
        }
    }

    #[test]
    fn reports_errors_from_more_than_one_connection() {
        let source = r#"
[connections.dev]
account = "bad"

[connections.prod]
policy = "write"
"#;
        let issues = Config::parse(source).expect_err("invalid connections");
        assert!(issues.iter().any(|issue| issue.contains("\"dev\"")));
        assert!(issues.iter().any(|issue| issue.contains("\"prod\"")));
    }

    #[test]
    fn rejects_an_empty_connection_set() {
        let issues = Config::parse("[connections]").expect_err("empty connections");
        assert_eq!(issues, ["connections must contain at least one connection"]);
    }

    #[test]
    fn renders_only_non_secret_connection_fields() {
        let config = Config::parse(VALID).expect("valid configuration");
        let output = config.render_connections();
        assert!(output.contains("dev\n"));
        assert!(output.contains("account: 111111111111"));
        assert!(output.contains("policy: read_only"));
        assert!(!output.contains("access_key"));
        assert!(!output.contains("secret"));
        assert!(!output.contains("token"));
    }
}
