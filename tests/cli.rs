use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

const VALID_CONFIG: &str = r#"
[connections.dev]
account = "111111111111"
region = "eu-west-1"
role_arn = "arn:aws:iam::111111111111:role/honk-readonly"
workgroup = "analytics-dev"
catalog = "AwsDataCatalog"
database = "analytics_dev"
policy = "read_only"
query_timeout = "30m"
"#;

fn home_with_config(source: &str) -> TempDir {
    let home = tempfile::tempdir().expect("temporary home");
    let directory = home.path().join(".config/honk");
    fs::create_dir_all(&directory).expect("configuration directory");
    fs::write(directory.join("config.toml"), source).expect("configuration file");
    home
}

fn write_aws_credentials(home: &Path, source: &str) {
    let directory = home.join(".aws");
    fs::create_dir_all(&directory).expect("AWS configuration directory");
    fs::write(directory.join("credentials"), source).expect("AWS credentials file");
}

fn honk(home: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_honk"))
        .args(arguments)
        .env("HOME", home)
        .output()
        .expect("run honk")
}

fn honk_with_stdin(home: &Path, arguments: &[&str], stdin: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_honk"))
        .args(arguments)
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start honk");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(stdin)
        .expect("write stdin");
    child.wait_with_output().expect("wait for honk")
}

#[test]
fn config_check_accepts_a_valid_file() {
    let home = home_with_config(VALID_CONFIG);
    let output = honk(home.path(), &["config", "check"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Configuration is valid"));
    assert!(output.stderr.is_empty());
}

#[test]
fn missing_configuration_is_exit_code_two() {
    let home = tempfile::tempdir().expect("temporary home");
    let output = honk(home.path(), &["config", "check"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot read configuration"));
    assert!(output.stdout.is_empty());
}

#[test]
fn config_check_reports_all_connection_errors() {
    let home = home_with_config(
        r#"
[connections.prod]
account = "wrong"
region = ""
role_arn = "also-wrong"
workgroup = ""
catalog = ""
database = ""
policy = "unrestricted"
query_timeout = "0s"
"#,
    );
    let output = honk(home.path(), &["config", "check"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("account"));
    assert!(stderr.contains("region"));
    assert!(stderr.contains("role_arn"));
    assert!(stderr.contains("workgroup"));
    assert!(stderr.contains("catalog"));
    assert!(stderr.contains("database"));
    assert!(stderr.contains("policy"));
    assert!(stderr.contains("query_timeout"));
    assert!(output.stdout.is_empty());
}

#[test]
fn configuration_errors_do_not_echo_values() {
    let home = home_with_config("connections = \"DO_NOT_PRINT_ME\"");
    let output = honk(home.path(), &["config", "check"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("DO_NOT_PRINT_ME"));
}

#[test]
fn connections_lists_safe_fields() {
    let home = home_with_config(VALID_CONFIG);
    let output = honk(home.path(), &["connections"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("dev"));
    assert!(stdout.contains("analytics-dev"));
    assert!(!stdout.contains("access_key"));
    assert!(!stdout.contains("secret"));
    assert!(!stdout.contains("token"));
}

#[test]
fn every_data_command_requires_connection_and_session() {
    let home = home_with_config(VALID_CONFIG);
    for arguments in [
        vec!["query", "SELECT 1"],
        vec!["catalogs"],
        vec!["databases"],
        vec!["tables"],
        vec!["describe", "analytics.orders"],
    ] {
        let output = honk(home.path(), &arguments);
        assert_eq!(output.status.code(), Some(2), "arguments: {arguments:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("--connection"), "arguments: {arguments:?}");
        assert!(stderr.contains("--session"), "arguments: {arguments:?}");
    }
}

#[test]
fn session_check_requires_connection_and_profile() {
    let home = home_with_config(VALID_CONFIG);
    let output = honk(home.path(), &["session", "check"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--connection"));
    assert!(stderr.contains("--session"));
}

#[test]
fn whitespace_connection_and_session_names_are_rejected() {
    let home = home_with_config(VALID_CONFIG);
    for arguments in [
        ["catalogs", "--connection", "  ", "--session", "dev-session"],
        ["catalogs", "--connection", "dev", "--session", "  "],
    ] {
        let output = honk(home.path(), &arguments);
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be empty"));
    }
}

#[test]
fn query_rejects_inline_sql_with_a_file() {
    let home = home_with_config(VALID_CONFIG);
    let output = honk(
        home.path(),
        &[
            "query",
            "--connection",
            "dev",
            "--session",
            "dev-session",
            "--file",
            "query.sql",
            "SELECT 1",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used with"));
}

#[test]
fn force_requires_an_output_path() {
    let home = home_with_config(VALID_CONFIG);
    let output = honk(
        home.path(),
        &[
            "query",
            "--connection",
            "dev",
            "--session",
            "dev-session",
            "--force",
            "SELECT 1",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--output"));
}

#[test]
fn phase_five_formats_reach_authentication() {
    let home = home_with_config(VALID_CONFIG);
    for format in ["table", "csv", "tsv", "json", "jsonl", "markdown"] {
        let output = honk(
            home.path(),
            &[
                "query",
                "--connection",
                "dev",
                "--session",
                "dev-session",
                "--format",
                format,
                "SELECT 1",
            ],
        );
        assert_eq!(output.status.code(), Some(3), "format: {format}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("AWS session profile"),
            "format: {format}"
        );
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn output_extension_rules_fail_before_authentication() {
    let home = home_with_config(VALID_CONFIG);
    let unknown = home.path().join("result.txt");
    let output = honk(
        home.path(),
        &[
            "query",
            "--connection",
            "dev",
            "--session",
            "dev-session",
            "--output",
            unknown.to_str().expect("path"),
            "SELECT 1",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot infer"));

    let csv = home.path().join("result.csv");
    let output = honk(
        home.path(),
        &[
            "query",
            "--connection",
            "dev",
            "--session",
            "dev-session",
            "--format",
            "jsonl",
            "--output",
            csv.to_str().expect("path"),
            "SELECT 1",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("conflicts"));
}

#[test]
fn output_file_requires_force_before_authentication() {
    let home = home_with_config(VALID_CONFIG);
    let path = home.path().join("result.csv");
    fs::write(&path, "existing").expect("existing file");
    let base = [
        "query",
        "--connection",
        "dev",
        "--session",
        "dev-session",
        "--output",
        path.to_str().expect("path"),
        "SELECT 1",
    ];
    let output = honk(home.path(), &base);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("already exists"));

    let mut forced = base[..base.len() - 1].to_vec();
    forced.push("--force");
    forced.push("SELECT 1");
    let output = honk(home.path(), &forced);
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(fs::read_to_string(path).expect("unchanged"), "existing");
}

#[test]
fn invalid_output_formats_are_invocation_errors() {
    let home = home_with_config(VALID_CONFIG);
    let output = honk(
        home.path(),
        &[
            "catalogs",
            "--connection",
            "dev",
            "--session",
            "dev-session",
            "--format",
            "xml",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid value 'xml'"));
    assert!(stderr.contains("jsonl"));
}

#[test]
fn describe_rejects_overqualified_names_before_authentication() {
    let home = home_with_config(VALID_CONFIG);
    let output = honk(
        home.path(),
        &[
            "describe",
            "--connection",
            "dev",
            "--session",
            "dev-session",
            "catalog.database.table",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("describe target must be TABLE or DATABASE.TABLE")
    );
}

#[test]
fn help_documents_the_phase_one_command_contract() {
    let home = home_with_config(VALID_CONFIG);
    let output = honk(home.path(), &["--help"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for command in [
        "query",
        "catalogs",
        "databases",
        "tables",
        "describe",
        "connections",
        "config",
        "session",
    ] {
        assert!(stdout.contains(command), "missing {command} from help");
    }
}

#[test]
fn valid_query_reaches_authentication_after_local_validation() {
    let home = home_with_config(VALID_CONFIG);
    let output = honk(
        home.path(),
        &[
            "query",
            "--connection",
            "dev",
            "--session",
            "dev-session",
            "SELECT 1",
        ],
    );
    assert_eq!(output.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("AWS session profile \"dev-session\" could not provide credentials")
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn query_rejects_writes_before_any_aws_phase() {
    let home = home_with_config(VALID_CONFIG);
    let output = honk(
        home.path(),
        &[
            "query",
            "--connection",
            "dev",
            "--session",
            "dev-session",
            "DELETE FROM orders",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("SQL policy rejected DELETE"));
    assert!(!stderr.contains("not implemented"));
    assert!(output.stdout.is_empty());
}

#[test]
fn query_reads_and_validates_a_sql_file() {
    let home = home_with_config(VALID_CONFIG);
    let query_path = home.path().join("investigation.sql");
    fs::write(&query_path, "SELECT count(*) FROM analytics.orders").expect("SQL file");
    let output = honk(
        home.path(),
        &[
            "query",
            "--connection",
            "dev",
            "--session",
            "dev-session",
            "--file",
            query_path.to_str().expect("UTF-8 path"),
        ],
    );
    assert_eq!(output.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("AWS session profile \"dev-session\" could not provide credentials")
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn query_reports_a_missing_sql_file() {
    let home = home_with_config(VALID_CONFIG);
    let missing = home.path().join("missing.sql");
    let output = honk(
        home.path(),
        &[
            "query",
            "--connection",
            "dev",
            "--session",
            "dev-session",
            "--file",
            missing.to_str().expect("UTF-8 path"),
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot read SQL file"));
    assert!(output.stdout.is_empty());
}

#[test]
fn query_reads_and_validates_stdin() {
    let home = home_with_config(VALID_CONFIG);
    let output = honk_with_stdin(
        home.path(),
        &["query", "--connection", "dev", "--session", "dev-session"],
        b"WITH source AS (SELECT 1 AS id) SELECT id FROM source",
    );
    assert_eq!(output.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("AWS session profile \"dev-session\" could not provide credentials")
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn empty_stdin_is_a_policy_error() {
    let home = home_with_config(VALID_CONFIG);
    let output = honk_with_stdin(
        home.path(),
        &["query", "--connection", "dev", "--session", "dev-session"],
        b"",
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("contains no statement"));
    assert!(output.stdout.is_empty());
}

#[test]
fn query_errors_do_not_echo_sql_literals() {
    let home = home_with_config(VALID_CONFIG);
    let output = honk(
        home.path(),
        &[
            "query",
            "--connection",
            "dev",
            "--session",
            "dev-session",
            "SELECT 'DO_NOT_PRINT_ME",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("could not be parsed"));
    assert!(!stderr.contains("DO_NOT_PRINT_ME"));
    assert!(output.stdout.is_empty());
}

#[test]
fn query_reports_the_known_iceberg_time_travel_gap() {
    let home = home_with_config(VALID_CONFIG);
    let output = honk(
        home.path(),
        &[
            "query",
            "--connection",
            "dev",
            "--session",
            "dev-session",
            "SELECT * FROM orders FOR VERSION AS OF 123",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("time travel is not supported"));
    assert!(output.stdout.is_empty());
}

#[test]
fn unknown_connections_are_named_without_contacting_aws() {
    let home = home_with_config(VALID_CONFIG);
    let output = honk(
        home.path(),
        &[
            "catalogs",
            "--connection",
            "prod",
            "--session",
            "prod-session",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown connection \"prod\""));
    assert!(stderr.contains("dev"));
}

#[test]
fn session_check_reports_missing_profile_as_authentication_failure() {
    let home = home_with_config(VALID_CONFIG);
    let output = honk(
        home.path(),
        &[
            "session",
            "check",
            "--connection",
            "dev",
            "--session",
            "dev-session",
        ],
    );
    assert_eq!(output.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("AWS session profile \"dev-session\" could not provide credentials")
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn static_profile_is_rejected_without_contacting_sts_or_leaking_credentials() {
    let home = home_with_config(VALID_CONFIG);
    write_aws_credentials(
        home.path(),
        r"
[dev-session]
aws_access_key_id = DO_NOT_PRINT_ACCESS
aws_secret_access_key = DO_NOT_PRINT_SECRET
",
    );
    let output = honk(
        home.path(),
        &[
            "session",
            "check",
            "--connection",
            "dev",
            "--session",
            "dev-session",
        ],
    );
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("contains long-lived credentials"));
    assert!(!stderr.contains("DO_NOT_PRINT"));
    assert!(output.stdout.is_empty());
}

#[test]
fn aws_profile_cannot_redirect_the_selected_session() {
    let home = home_with_config(VALID_CONFIG);
    write_aws_credentials(
        home.path(),
        r"
[default]
aws_access_key_id = incomplete-default

[dev-session]
aws_access_key_id = DO_NOT_PRINT_ACCESS
aws_secret_access_key = DO_NOT_PRINT_SECRET
",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_honk"))
        .args([
            "session",
            "check",
            "--connection",
            "dev",
            "--session",
            "dev-session",
        ])
        .env("HOME", home.path())
        .env("AWS_PROFILE", "default")
        .output()
        .expect("run honk");
    assert_eq!(output.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&output.stderr).contains("long-lived credentials"));
    assert!(output.stdout.is_empty());
}

#[test]
fn exported_credentials_cannot_replace_the_selected_session() {
    let home = home_with_config(VALID_CONFIG);
    write_aws_credentials(
        home.path(),
        r"
[dev-session]
aws_access_key_id = incomplete-selected-profile
",
    );
    let output = Command::new(env!("CARGO_BIN_EXE_honk"))
        .args([
            "session",
            "check",
            "--connection",
            "dev",
            "--session",
            "dev-session",
        ])
        .env("HOME", home.path())
        .env("AWS_ACCESS_KEY_ID", "DO_NOT_PRINT_ACCESS")
        .env("AWS_SECRET_ACCESS_KEY", "DO_NOT_PRINT_SECRET")
        .output()
        .expect("run honk");
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("could not provide credentials"));
    assert!(!stderr.contains("DO_NOT_PRINT"));
    assert!(output.stdout.is_empty());
}

#[test]
fn shared_credentials_file_override_cannot_redirect_the_selected_session() {
    let home = home_with_config(VALID_CONFIG);
    write_aws_credentials(
        home.path(),
        r"
[dev-session]
aws_access_key_id = DO_NOT_PRINT_ACCESS
aws_secret_access_key = DO_NOT_PRINT_SECRET
",
    );
    let alternative = home.path().join("alternative-credentials");
    fs::write(
        &alternative,
        r"
[dev-session]
aws_access_key_id = incomplete-alternative
",
    )
    .expect("alternative credentials file");

    let output = Command::new(env!("CARGO_BIN_EXE_honk"))
        .args([
            "session",
            "check",
            "--connection",
            "dev",
            "--session",
            "dev-session",
        ])
        .env("HOME", home.path())
        .env("AWS_SHARED_CREDENTIALS_FILE", alternative)
        .output()
        .expect("run honk");
    assert_eq!(output.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&output.stderr).contains("long-lived credentials"));
    assert!(output.stdout.is_empty());
}
