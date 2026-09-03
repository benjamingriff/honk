use std::collections::{HashMap, HashSet};

use crate::athena::api::ResultColumn;

use super::values::{Value, ValueKind};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Column {
    pub(crate) name: String,
    kind: ValueKind,
}

impl Column {
    pub(crate) fn value<'a>(&self, raw: Option<&'a str>) -> Value<'a> {
        self.kind.convert(raw)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Rename {
    pub(crate) original: String,
    pub(crate) output: String,
}

pub(crate) fn rename_columns(columns: &[ResultColumn]) -> (Vec<Column>, Vec<Rename>) {
    let mut used = HashSet::new();
    let mut counts = HashMap::<String, usize>::new();
    let mut renamed = Vec::new();
    let output = columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            let base = if column.name.trim().is_empty() {
                format!("_col{}", index + 1)
            } else {
                column.name.clone()
            };
            let count = counts.entry(base.clone()).or_insert(0);
            let mut candidate = base.clone();
            loop {
                *count += 1;
                if *count > 1 {
                    candidate = format!("{base}_{count}");
                }
                if used.insert(candidate.clone()) {
                    break;
                }
            }
            if candidate != column.name {
                renamed.push(Rename {
                    original: column.name.clone(),
                    output: candidate.clone(),
                });
            }
            Column {
                name: candidate,
                kind: ValueKind::parse(&column.data_type),
            }
        })
        .collect();
    (output, renamed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str) -> ResultColumn {
        ResultColumn {
            name: name.to_owned(),
            data_type: "varchar".to_owned(),
        }
    }

    #[test]
    fn makes_blank_and_duplicate_names_unique_without_collisions() {
        let (columns, renames) = rename_columns(&[
            column(""),
            column("name"),
            column("name"),
            column("name_2"),
            column("name"),
        ]);
        assert_eq!(
            columns
                .iter()
                .map(|value| value.name.as_str())
                .collect::<Vec<_>>(),
            ["_col1", "name", "name_2", "name_2_2", "name_3"]
        );
        assert_eq!(renames.len(), 4);
    }
}
