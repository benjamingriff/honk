use std::fs::File;
use std::io::{self, IsTerminal as _, Read as _};

use crate::cli::{DataArgs, NamespaceArgs, QueryArgs};
use crate::error::AppError;
use crate::policy::athena::{MAX_SQL_BYTES, ValidatedQuery};

pub struct PreparedQuery {
    pub data: DataArgs,
    pub namespace: NamespaceArgs,
    pub query: ValidatedQuery,
}

pub fn prepare(arguments: QueryArgs) -> Result<PreparedQuery, AppError> {
    let sql = match (&arguments.file, arguments.sql) {
        (Some(path), None) => {
            let file = File::open(path).map_err(|source| AppError::ReadSqlFile {
                path: path.clone(),
                source,
            })?;
            read_utf8_limited(file, format!("SQL file {}", path.display()))?
        }
        (None, Some(sql)) => sql,
        (None, None) => {
            let stdin = io::stdin();
            if stdin.is_terminal() {
                return Err(AppError::MissingSqlInput);
            }
            read_utf8_limited(stdin.lock(), "stdin".to_owned())?
        }
        (Some(_), Some(_)) => unreachable!("clap rejects --file with positional SQL"),
    };

    let query = ValidatedQuery::parse(sql)?;
    Ok(PreparedQuery {
        data: arguments.data,
        namespace: arguments.namespace,
        query,
    })
}

fn read_utf8_limited(reader: impl io::Read, source_name: String) -> Result<String, AppError> {
    let limit = u64::try_from(MAX_SQL_BYTES).expect("SQL limit fits in u64") + 1;
    let mut bytes = Vec::new();
    reader
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(|source| AppError::ReadSqlInput {
            source_name: source_name.clone(),
            source,
        })?;

    if bytes.len() > MAX_SQL_BYTES {
        return Err(crate::policy::athena::PolicyError::InputTooLarge {
            limit: MAX_SQL_BYTES,
        }
        .into());
    }

    String::from_utf8(bytes).map_err(|_| AppError::SqlInputNotUtf8 { source_name })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn limited_reader_stops_after_the_first_excess_byte() {
        let input = vec![b'x'; MAX_SQL_BYTES + 100];
        let error = read_utf8_limited(Cursor::new(input), "test input".to_owned())
            .expect_err("oversized input");
        assert!(error.to_string().contains("1048576-byte limit"));
    }

    #[test]
    fn limited_reader_rejects_non_utf8_input_without_echoing_it() {
        let error = read_utf8_limited(Cursor::new([0xff]), "test input".to_owned())
            .expect_err("invalid UTF-8");
        assert_eq!(error.to_string(), "test input is not valid UTF-8");
    }
}
