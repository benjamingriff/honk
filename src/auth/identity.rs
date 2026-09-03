use aws_config::BehaviorVersion;
use aws_credential_types::provider::SharedCredentialsProvider;
use aws_sdk_sts::config::Region;
use aws_sdk_sts::error::ProvideErrorMetadata as _;

use super::IdentityFuture;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CallerIdentity {
    pub(super) account: Option<String>,
    pub(super) arn: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum IdentityLookupError {
    Expired,
    Unavailable,
}

pub(super) trait IdentityVerifier: Send + Sync {
    fn get_caller_identity<'a>(
        &'a self,
        provider: SharedCredentialsProvider,
        region: &'a str,
    ) -> IdentityFuture<'a>;
}

#[derive(Clone, Copy, Debug)]
pub(super) struct StsIdentityVerifier;

impl IdentityVerifier for StsIdentityVerifier {
    fn get_caller_identity<'a>(
        &'a self,
        provider: SharedCredentialsProvider,
        region: &'a str,
    ) -> IdentityFuture<'a> {
        Box::pin(async move {
            let sdk_config = aws_config::defaults(BehaviorVersion::latest())
                .empty_test_environment()
                .region(Region::new(region.to_owned()))
                .credentials_provider(provider)
                .load()
                .await;
            let result = aws_sdk_sts::Client::new(&sdk_config)
                .get_caller_identity()
                .send()
                .await
                .map_err(|error| {
                    let code = error.as_service_error().and_then(|value| value.code());
                    classify_error_code(code)
                })?;
            Ok(CallerIdentity {
                account: result.account().map(str::to_owned),
                arn: result.arn().map(str::to_owned),
            })
        })
    }
}

fn classify_error_code(code: Option<&str>) -> IdentityLookupError {
    if super::is_expired_aws_error_code(code) {
        IdentityLookupError::Expired
    } else {
        IdentityLookupError::Unavailable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_only_expiry_codes_as_expired() {
        for code in ["ExpiredToken", "ExpiredTokenException", "RequestExpired"] {
            assert_eq!(
                classify_error_code(Some(code)),
                IdentityLookupError::Expired
            );
        }
        assert_eq!(
            classify_error_code(Some("InvalidClientTokenId")),
            IdentityLookupError::Unavailable
        );
        assert_eq!(classify_error_code(None), IdentityLookupError::Unavailable);
    }
}
