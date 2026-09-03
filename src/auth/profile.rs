use std::borrow::Cow;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use aws_credential_types::Credentials;
use aws_credential_types::provider::SharedCredentialsProvider;
use aws_runtime::env_config::file::{
    EnvConfigFileKind as ProfileFileKind, EnvConfigFiles as ProfileFiles,
};
use aws_types::os_shim_internal::{Env, Fs};

pub(super) type ProfileFuture<'a> =
    Pin<Box<dyn Future<Output = Result<SharedCredentialsProvider, ProfileLoadError>> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ProfileLoadError {
    Unavailable,
}

pub(super) trait SessionProfileLoader: Send + Sync {
    fn load<'a>(&'a self, profile: &'a str) -> ProfileFuture<'a>;
}

#[derive(Clone, Debug)]
pub(super) struct AwsProfileLoader {
    profile_files: ProfileFiles,
}

impl AwsProfileLoader {
    pub(super) fn new(credentials_file: PathBuf) -> Self {
        let profile_files = ProfileFiles::builder()
            .with_file(ProfileFileKind::Credentials, credentials_file)
            .build();
        Self { profile_files }
    }

    #[cfg(test)]
    pub(super) fn from_profile_files(profile_files: ProfileFiles) -> Self {
        Self { profile_files }
    }
}

impl SessionProfileLoader for AwsProfileLoader {
    fn load<'a>(&'a self, profile: &'a str) -> ProfileFuture<'a> {
        Box::pin(async move {
            let profiles = aws_config::profile::load(
                &Fs::real(),
                &Env::from_slice(&[]),
                &self.profile_files,
                Some(Cow::Owned(profile.to_owned())),
            )
            .await
            .map_err(|_| ProfileLoadError::Unavailable)?;
            let selected = profiles
                .get_profile(profile)
                .ok_or(ProfileLoadError::Unavailable)?;
            let access_key = selected
                .get("aws_access_key_id")
                .filter(|value| !value.is_empty())
                .ok_or(ProfileLoadError::Unavailable)?;
            let secret_key = selected
                .get("aws_secret_access_key")
                .filter(|value| !value.is_empty())
                .ok_or(ProfileLoadError::Unavailable)?;
            let session_token = selected
                .get("aws_session_token")
                .filter(|value| !value.is_empty())
                .map(str::to_owned);

            let credentials = Credentials::new(
                access_key,
                secret_key,
                session_token,
                None,
                "honk-explicit-session-profile",
            );
            Ok(SharedCredentialsProvider::new(credentials))
        })
    }
}
