mod identity;
mod profile;

use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::{Duration, SystemTime};

use aws_credential_types::provider::{ProvideCredentials as _, SharedCredentialsProvider};
use thiserror::Error;

use self::identity::{CallerIdentity, IdentityLookupError, IdentityVerifier};
use self::profile::{AwsProfileLoader, SessionProfileLoader};
use crate::config::Connection;

pub(crate) const SESSION_EXPIRY_MARGIN: Duration = Duration::from_mins(5);

pub(crate) fn is_expired_aws_error_code(code: Option<&str>) -> bool {
    matches!(
        code,
        Some("ExpiredToken" | "ExpiredTokenException" | "RequestExpired")
    )
}

type IdentityFuture<'a> =
    Pin<Box<dyn Future<Output = Result<CallerIdentity, IdentityLookupError>> + Send + 'a>>;

#[derive(Clone)]
pub(crate) struct VerifiedSession {
    profile: String,
    provider: SharedCredentialsProvider,
    account: String,
    role_name: String,
}

impl VerifiedSession {
    pub(crate) fn account(&self) -> &str {
        &self.account
    }

    pub(crate) fn role_name(&self) -> &str {
        &self.role_name
    }

    #[allow(dead_code, reason = "Phase 4 will build Athena from this provider")]
    pub(crate) fn credentials_provider(&self) -> SharedCredentialsProvider {
        self.provider.clone()
    }
}

impl fmt::Debug for VerifiedSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedSession")
            .field("profile", &self.profile)
            .field("account", &self.account)
            .field("role_name", &self.role_name)
            .field("provider", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum AuthError {
    #[error(
        "AWS session profile {profile:?} could not provide credentials; recreate or refresh it"
    )]
    ProfileUnavailable { profile: String },

    #[error(
        "AWS session profile {profile:?} contains long-lived credentials; Honk requires a session token"
    )]
    StaticCredentials { profile: String },

    #[error(
        "AWS session profile {profile:?} expires too soon for connection {connection:?}; recreate or refresh it"
    )]
    ExpiresTooSoon { profile: String, connection: String },

    #[error(
        "AWS session profile {profile:?} has expired or invalid credentials; recreate or refresh it"
    )]
    Expired { profile: String },

    #[error(
        "STS could not verify AWS session profile {profile:?}; recreate or refresh it and try again"
    )]
    IdentityUnavailable { profile: String },

    #[error("STS returned an incomplete identity for AWS session profile {profile:?}")]
    IncompleteIdentity { profile: String },

    #[error(
        "AWS session profile {profile:?} resolved to account {actual:?}, but connection {connection:?} requires {expected:?}"
    )]
    WrongAccount {
        profile: String,
        connection: String,
        expected: String,
        actual: String,
    },

    #[error(
        "AWS session profile {profile:?} resolved to role {actual:?}, but connection {connection:?} requires {expected:?}"
    )]
    WrongRole {
        profile: String,
        connection: String,
        expected: String,
        actual: String,
    },
}

pub(crate) async fn verify_session(
    connection: &Connection,
    profile: &str,
    credentials_file: PathBuf,
) -> Result<VerifiedSession, AuthError> {
    let loader = AwsProfileLoader::new(credentials_file);
    let identity = identity::StsIdentityVerifier;
    verify_session_with(&loader, &identity, connection, profile, SystemTime::now()).await
}

async fn verify_session_with<L, I>(
    loader: &L,
    identity_verifier: &I,
    connection: &Connection,
    profile: &str,
    now: SystemTime,
) -> Result<VerifiedSession, AuthError>
where
    L: SessionProfileLoader,
    I: IdentityVerifier,
{
    let provider = loader
        .load(profile)
        .await
        .map_err(|_| AuthError::ProfileUnavailable {
            profile: profile.to_owned(),
        })?;
    let credentials =
        provider
            .provide_credentials()
            .await
            .map_err(|_| AuthError::ProfileUnavailable {
                profile: profile.to_owned(),
            })?;

    if credentials.session_token().is_none_or(str::is_empty) {
        return Err(AuthError::StaticCredentials {
            profile: profile.to_owned(),
        });
    }

    if let Some(expiry) = credentials.expiry() {
        let Some(required_lifetime) = connection.query_timeout.checked_add(SESSION_EXPIRY_MARGIN)
        else {
            return Err(AuthError::ExpiresTooSoon {
                profile: profile.to_owned(),
                connection: connection.name.clone(),
            });
        };
        let Some(required_until) = now.checked_add(required_lifetime) else {
            return Err(AuthError::ExpiresTooSoon {
                profile: profile.to_owned(),
                connection: connection.name.clone(),
            });
        };
        if expiry <= required_until {
            return Err(AuthError::ExpiresTooSoon {
                profile: profile.to_owned(),
                connection: connection.name.clone(),
            });
        }
    }
    // Freeze the exact SDK credential object that passed the local checks. The
    // profile file may be refreshed concurrently, but one Honk invocation must
    // not validate one credential set and send another to AWS.
    let checked_provider = SharedCredentialsProvider::new(credentials);

    let identity = identity_verifier
        .get_caller_identity(checked_provider.clone(), &connection.region)
        .await
        .map_err(|error| match error {
            IdentityLookupError::Expired => AuthError::Expired {
                profile: profile.to_owned(),
            },
            IdentityLookupError::Unavailable => AuthError::IdentityUnavailable {
                profile: profile.to_owned(),
            },
        })?;

    let Some(account) = identity.account else {
        return Err(AuthError::IncompleteIdentity {
            profile: profile.to_owned(),
        });
    };
    let Some(arn) = identity.arn else {
        return Err(AuthError::IncompleteIdentity {
            profile: profile.to_owned(),
        });
    };
    if account != connection.account {
        return Err(AuthError::WrongAccount {
            profile: profile.to_owned(),
            connection: connection.name.clone(),
            expected: connection.account.clone(),
            actual: account,
        });
    }

    let Some((arn_account, role_name)) = parse_assumed_role_arn(&arn) else {
        return Err(AuthError::IncompleteIdentity {
            profile: profile.to_owned(),
        });
    };
    if arn_account != account {
        return Err(AuthError::IncompleteIdentity {
            profile: profile.to_owned(),
        });
    }

    let Some(expected_role) = connection.expected_role_name() else {
        return Err(AuthError::IncompleteIdentity {
            profile: profile.to_owned(),
        });
    };
    if role_name != expected_role {
        return Err(AuthError::WrongRole {
            profile: profile.to_owned(),
            connection: connection.name.clone(),
            expected: expected_role.to_owned(),
            actual: role_name.to_owned(),
        });
    }

    Ok(VerifiedSession {
        profile: profile.to_owned(),
        provider: checked_provider,
        account,
        role_name: role_name.to_owned(),
    })
}

fn parse_assumed_role_arn(arn: &str) -> Option<(&str, &str)> {
    let parts = arn.splitn(6, ':').collect::<Vec<_>>();
    if parts.len() != 6
        || parts[0] != "arn"
        || parts[1].is_empty()
        || parts[2] != "sts"
        || !parts[3].is_empty()
        || parts[4].len() != 12
        || !parts[4].bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }

    let resource = parts[5].strip_prefix("assumed-role/")?;
    let mut resource_parts = resource.split('/');
    let role_name = resource_parts.next().filter(|value| !value.is_empty())?;
    resource_parts.next().filter(|value| !value.is_empty())?;
    if resource_parts.next().is_some() {
        return None;
    }
    Some((parts[4], role_name))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use aws_credential_types::Credentials;
    use aws_runtime::env_config::file::{EnvConfigFileKind, EnvConfigFiles};

    use super::*;
    use crate::config::Policy;

    const NOW: SystemTime = SystemTime::UNIX_EPOCH;

    #[derive(Clone)]
    struct FixedLoader {
        credentials: Credentials,
    }

    impl SessionProfileLoader for FixedLoader {
        fn load<'a>(&'a self, _profile: &'a str) -> profile::ProfileFuture<'a> {
            let provider = SharedCredentialsProvider::new(self.credentials.clone());
            Box::pin(async move { Ok(provider) })
        }
    }

    struct MockIdentityVerifier {
        result: Result<CallerIdentity, IdentityLookupError>,
        calls: AtomicUsize,
        regions: Mutex<Vec<String>>,
    }

    impl MockIdentityVerifier {
        fn returning(result: Result<CallerIdentity, IdentityLookupError>) -> Self {
            Self {
                result,
                calls: AtomicUsize::new(0),
                regions: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl IdentityVerifier for MockIdentityVerifier {
        fn get_caller_identity<'a>(
            &'a self,
            _provider: SharedCredentialsProvider,
            region: &'a str,
        ) -> IdentityFuture<'a> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.regions
                .lock()
                .expect("region lock")
                .push(region.to_owned());
            let result = self.result.clone();
            Box::pin(async move { result })
        }
    }

    fn connection() -> Connection {
        Connection {
            name: "dev".to_owned(),
            account: "111111111111".to_owned(),
            region: "eu-west-1".to_owned(),
            role_arn: "arn:aws:iam::111111111111:role/honk-readonly".to_owned(),
            workgroup: "analytics-dev".to_owned(),
            catalog: "AwsDataCatalog".to_owned(),
            database: "analytics_dev".to_owned(),
            output_location: None,
            policy: Policy::ReadOnly,
            query_timeout: Duration::from_mins(30),
        }
    }

    fn credentials(token: Option<&str>, expiry: Option<SystemTime>) -> Credentials {
        Credentials::new(
            "test-access-key",
            "DO_NOT_PRINT_SECRET",
            token.map(str::to_owned),
            expiry,
            "honk-test",
        )
    }

    fn valid_identity(session_name: &str) -> CallerIdentity {
        CallerIdentity {
            account: Some("111111111111".to_owned()),
            arn: Some(format!(
                "arn:aws:sts::111111111111:assumed-role/honk-readonly/{session_name}"
            )),
        }
    }

    fn loader_with_contents(contents: &str) -> AwsProfileLoader {
        let files = EnvConfigFiles::builder()
            .with_contents(EnvConfigFileKind::Credentials, contents)
            .build();
        AwsProfileLoader::from_profile_files(files)
    }

    #[tokio::test]
    async fn explicit_profile_name_wins_over_default() {
        let loader = loader_with_contents(
            r"
[default]
aws_access_key_id = default-access
aws_secret_access_key = default-secret

[wanted-session]
aws_access_key_id = wanted-access
aws_secret_access_key = wanted-secret
aws_session_token = wanted-token
",
        );
        let identity = MockIdentityVerifier::returning(Ok(valid_identity("first")));
        let session = verify_session_with(&loader, &identity, &connection(), "wanted-session", NOW)
            .await
            .expect("wanted profile");
        assert_eq!(session.account(), "111111111111");

        let default_error = verify_session_with(&loader, &identity, &connection(), "default", NOW)
            .await
            .expect_err("default profile is static");
        assert!(matches!(default_error, AuthError::StaticCredentials { .. }));
    }

    #[tokio::test]
    async fn missing_and_incomplete_profiles_fail_before_identity_lookup() {
        let loader = loader_with_contents(
            r"
[incomplete]
aws_access_key_id = test-access
",
        );
        let identity = MockIdentityVerifier::returning(Ok(valid_identity("unused")));

        for profile in ["missing", "incomplete"] {
            let error = verify_session_with(&loader, &identity, &connection(), profile, NOW)
                .await
                .expect_err("profile must fail");
            assert_eq!(
                error,
                AuthError::ProfileUnavailable {
                    profile: profile.to_owned()
                }
            );
        }
        assert_eq!(identity.calls(), 0);
    }

    #[tokio::test]
    async fn indirect_profile_sources_are_not_executed_or_followed() {
        let loader = loader_with_contents(
            r"
[process]
credential_process = /DO_NOT_RUN

[role-chain]
role_arn = arn:aws:iam::111111111111:role/honk-readonly
source_profile = source

[source]
aws_access_key_id = source-access
aws_secret_access_key = source-secret
aws_session_token = source-token
",
        );
        let identity = MockIdentityVerifier::returning(Ok(valid_identity("unused")));

        for profile in ["process", "role-chain"] {
            let error = verify_session_with(&loader, &identity, &connection(), profile, NOW)
                .await
                .expect_err("indirect credential source must fail");
            assert_eq!(
                error,
                AuthError::ProfileUnavailable {
                    profile: profile.to_owned()
                }
            );
        }
        assert_eq!(identity.calls(), 0);
    }

    #[tokio::test]
    async fn static_credentials_fail_before_identity_lookup() {
        let loader = FixedLoader {
            credentials: credentials(None, None),
        };
        let identity = MockIdentityVerifier::returning(Ok(valid_identity("unused")));
        let error = verify_session_with(&loader, &identity, &connection(), "static", NOW)
            .await
            .expect_err("static credentials");
        assert_eq!(
            error,
            AuthError::StaticCredentials {
                profile: "static".to_owned()
            }
        );
        assert_eq!(identity.calls(), 0);
    }

    #[tokio::test]
    async fn known_expiry_must_cover_timeout_and_margin() {
        let expiry = NOW + Duration::from_mins(35);
        let loader = FixedLoader {
            credentials: credentials(Some("test-token"), Some(expiry)),
        };
        let identity = MockIdentityVerifier::returning(Ok(valid_identity("unused")));
        let error = verify_session_with(&loader, &identity, &connection(), "short", NOW)
            .await
            .expect_err("expiry at the boundary");
        assert!(matches!(error, AuthError::ExpiresTooSoon { .. }));
        assert_eq!(identity.calls(), 0);
    }

    #[tokio::test]
    async fn unknown_and_sufficient_expiry_proceed_to_identity_lookup() {
        for expiry in [None, Some(NOW + Duration::from_mins(36))] {
            let loader = FixedLoader {
                credentials: credentials(Some("test-token"), expiry),
            };
            let identity = MockIdentityVerifier::returning(Ok(valid_identity("agent-run")));
            let session =
                verify_session_with(&loader, &identity, &connection(), "dev-session", NOW)
                    .await
                    .expect("valid session");
            assert_eq!(session.role_name(), "honk-readonly");
            assert_eq!(identity.calls(), 1);
            assert_eq!(
                identity.regions.lock().expect("region lock").as_slice(),
                ["eu-west-1"]
            );
        }
    }

    #[tokio::test]
    async fn expired_sts_response_is_classified_without_sdk_text() {
        let loader = FixedLoader {
            credentials: credentials(Some("DO_NOT_PRINT_TOKEN"), None),
        };
        let identity = MockIdentityVerifier::returning(Err(IdentityLookupError::Expired));
        let error = verify_session_with(&loader, &identity, &connection(), "dev-session", NOW)
            .await
            .expect_err("expired token");
        assert_eq!(
            error,
            AuthError::Expired {
                profile: "dev-session".to_owned()
            }
        );
        assert!(!error.to_string().contains("DO_NOT_PRINT"));
    }

    #[tokio::test]
    async fn wrong_account_and_role_are_rejected() {
        let loader = FixedLoader {
            credentials: credentials(Some("test-token"), None),
        };
        let wrong_account = MockIdentityVerifier::returning(Ok(CallerIdentity {
            account: Some("222222222222".to_owned()),
            arn: Some("arn:aws:sts::222222222222:assumed-role/honk-readonly/session".to_owned()),
        }));
        let error = verify_session_with(&loader, &wrong_account, &connection(), "dev-session", NOW)
            .await
            .expect_err("wrong account");
        assert!(matches!(error, AuthError::WrongAccount { .. }));

        let wrong_role = MockIdentityVerifier::returning(Ok(CallerIdentity {
            account: Some("111111111111".to_owned()),
            arn: Some("arn:aws:sts::111111111111:assumed-role/admin/session".to_owned()),
        }));
        let error = verify_session_with(&loader, &wrong_role, &connection(), "dev-session", NOW)
            .await
            .expect_err("wrong role");
        assert!(matches!(error, AuthError::WrongRole { .. }));
    }

    #[tokio::test]
    async fn varied_role_session_names_do_not_affect_role_matching() {
        let loader = FixedLoader {
            credentials: credentials(Some("test-token"), None),
        };
        for session_name in ["human", "agent-20260901", "name.with_symbols+=,@"] {
            let identity = MockIdentityVerifier::returning(Ok(valid_identity(session_name)));
            verify_session_with(&loader, &identity, &connection(), "dev-session", NOW)
                .await
                .expect("role session name is ignored");
        }
    }

    #[tokio::test]
    async fn malformed_or_non_role_identity_is_incomplete() {
        let loader = FixedLoader {
            credentials: credentials(Some("test-token"), None),
        };
        for arn in [
            None,
            Some("arn:aws:iam::111111111111:user/example"),
            Some("arn:aws:sts::111111111111:assumed-role/honk-readonly"),
            Some("arn:aws:sts::222222222222:assumed-role/honk-readonly/session"),
        ] {
            let identity = MockIdentityVerifier::returning(Ok(CallerIdentity {
                account: Some("111111111111".to_owned()),
                arn: arn.map(str::to_owned),
            }));
            let error = verify_session_with(&loader, &identity, &connection(), "dev-session", NOW)
                .await
                .expect_err("incomplete identity");
            assert!(matches!(error, AuthError::IncompleteIdentity { .. }));
        }
    }

    #[tokio::test]
    async fn configured_role_paths_compare_by_role_name() {
        let mut connection = connection();
        connection.role_arn =
            "arn:aws:iam::111111111111:role/company/data/honk-readonly".to_owned();
        let loader = FixedLoader {
            credentials: credentials(Some("test-token"), None),
        };
        let identity = MockIdentityVerifier::returning(Ok(valid_identity("agent")));
        verify_session_with(&loader, &identity, &connection, "dev-session", NOW)
            .await
            .expect("role name matches");
    }

    #[tokio::test]
    async fn debug_and_errors_do_not_contain_credentials() {
        let loader = FixedLoader {
            credentials: credentials(Some("DO_NOT_PRINT_TOKEN"), None),
        };
        let identity = MockIdentityVerifier::returning(Ok(valid_identity("agent")));
        let session = verify_session_with(&loader, &identity, &connection(), "dev-session", NOW)
            .await
            .expect("valid session");
        let debug = format!("{session:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("DO_NOT_PRINT"));
        assert!(!debug.contains("test-access-key"));
    }

    #[tokio::test]
    async fn parallel_verification_reads_without_mutating_the_credentials_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("credentials");
        let contents = r"
[dev-session]
aws_access_key_id = test-access
aws_secret_access_key = DO_NOT_PRINT_SECRET
aws_session_token = DO_NOT_PRINT_TOKEN
";
        fs::write(&path, contents).expect("credentials fixture");
        let before = fs::read(&path).expect("credentials before");
        let loader = AwsProfileLoader::new(path.clone());
        let identity = MockIdentityVerifier::returning(Ok(valid_identity("parallel")));
        let connection = connection();

        let first = verify_session_with(&loader, &identity, &connection, "dev-session", NOW);
        let second = verify_session_with(&loader, &identity, &connection, "dev-session", NOW);
        let (first, second) = tokio::join!(first, second);
        first.expect("first verification");
        second.expect("second verification");

        let after = fs::read(&path).expect("credentials after");
        assert_eq!(before, after);
        assert_eq!(identity.calls(), 2);
    }
}
