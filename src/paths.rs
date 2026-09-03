use std::path::PathBuf;

use crate::error::AppError;

pub(crate) fn config_file() -> Result<PathBuf, AppError> {
    Ok(home_directory()?.join(".config/honk/config.toml"))
}

pub(crate) fn aws_credentials_file() -> Result<PathBuf, AppError> {
    Ok(home_directory()?.join(".aws/credentials"))
}

fn home_directory() -> Result<PathBuf, AppError> {
    let home = std::env::var_os("HOME").filter(|value| !value.is_empty());
    home.map(PathBuf::from).ok_or(AppError::MissingHome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_the_documented_relative_path() {
        let home = PathBuf::from("/Users/example");
        let path = home.join(".config/honk/config.toml");
        assert_eq!(
            path,
            PathBuf::from("/Users/example/.config/honk/config.toml")
        );
        assert_eq!(
            home.join(".aws/credentials"),
            PathBuf::from("/Users/example/.aws/credentials")
        );
    }
}
