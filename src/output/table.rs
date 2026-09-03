use std::io::{self, Write};

use super::Column;

const MIN_CELL_WIDTH: usize = 8;
const MAX_CELL_WIDTH: usize = 30;
const DEFAULT_WIDTH: usize = 120;

pub(super) struct Table {
    layout: Layout,
    rows: usize,
}

enum Layout {
    Horizontal {
        widths: Vec<usize>,
    },
    Expanded {
        label_width: usize,
        total_width: usize,
    },
}

impl Table {
    pub(super) fn begin(
        writer: &mut dyn Write,
        columns: &[Column],
        width: Option<usize>,
    ) -> io::Result<Self> {
        let total_width = width.unwrap_or(DEFAULT_WIDTH).max(20);
        let overhead = columns.len().saturating_mul(3).saturating_add(1);
        let minimum = columns
            .len()
            .saturating_mul(MIN_CELL_WIDTH)
            .saturating_add(overhead);
        let layout = if !columns.is_empty() && minimum <= total_width {
            let widths = allocate_widths(columns, total_width.saturating_sub(overhead));
            write_rule(writer, &widths)?;
            write_horizontal_row(
                writer,
                &columns
                    .iter()
                    .map(|column| column.name.as_str())
                    .collect::<Vec<_>>(),
                &widths,
            )?;
            write_rule(writer, &widths)?;
            Layout::Horizontal { widths }
        } else {
            let label_width = columns
                .iter()
                .map(|column| display_width(&clean(&column.name)))
                .max()
                .unwrap_or(0)
                .min(MAX_CELL_WIDTH);
            Layout::Expanded {
                label_width,
                total_width,
            }
        };
        Ok(Self { layout, rows: 0 })
    }

    pub(super) fn row(
        &mut self,
        writer: &mut dyn Write,
        columns: &[Column],
        raw: &[Option<String>],
    ) -> io::Result<()> {
        self.rows += 1;
        match &self.layout {
            Layout::Horizontal { widths } => {
                let cells = raw_cells(raw, columns.len());
                write_horizontal_row(
                    writer,
                    &cells.iter().map(String::as_str).collect::<Vec<_>>(),
                    widths,
                )
            }
            Layout::Expanded {
                label_width,
                total_width,
            } => {
                let title = format!("-[ Row {} ]", self.rows);
                writeln!(writer, "{}", fill_to_width(&title, *total_width, '-'))?;
                for (index, column) in columns.iter().enumerate() {
                    let label = truncate(&clean(&column.name), *label_width);
                    let value = raw
                        .get(index)
                        .and_then(Option::as_deref)
                        .map_or_else(|| "NULL".to_owned(), clean);
                    let value_width = total_width.saturating_sub(*label_width + 3).max(1);
                    writeln!(
                        writer,
                        "{label}{} | {}",
                        " ".repeat(label_width.saturating_sub(display_width(&label))),
                        truncate(&value, value_width)
                    )?;
                }
                Ok(())
            }
        }
    }

    pub(super) fn finish(&self, writer: &mut dyn Write) -> io::Result<()> {
        match &self.layout {
            Layout::Horizontal { widths } => write_rule(writer, widths),
            Layout::Expanded { .. } if self.rows == 0 => writer.write_all(b"(0 rows)\n"),
            Layout::Expanded { .. } => Ok(()),
        }
    }
}

fn allocate_widths(columns: &[Column], available: usize) -> Vec<usize> {
    let mut widths = vec![MIN_CELL_WIDTH; columns.len()];
    let targets = columns
        .iter()
        .map(|column| display_width(&clean(&column.name)).clamp(16, MAX_CELL_WIDTH))
        .collect::<Vec<_>>();
    let mut left = available.saturating_sub(widths.iter().sum());
    while left > 0 {
        let mut changed = false;
        for (width, target) in widths.iter_mut().zip(&targets) {
            if left == 0 {
                break;
            }
            if *width < *target {
                *width += 1;
                left -= 1;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    widths
}

fn raw_cells(raw: &[Option<String>], count: usize) -> Vec<String> {
    (0..count)
        .map(|index| {
            raw.get(index)
                .and_then(Option::as_deref)
                .map_or_else(|| "NULL".to_owned(), clean)
        })
        .collect()
}

fn clean(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => output.push_str("\\\\"),
            '|' => output.push_str("\\|"),
            '\r' => output.push_str("\\r"),
            '\n' => output.push_str("\\n"),
            '\t' => output.push_str("\\t"),
            value if value.is_control() => output.extend(value.escape_unicode()),
            value => output.push(value),
        }
    }
    output
}

fn display_width(value: &str) -> usize {
    value.chars().count()
}

fn truncate(value: &str, width: usize) -> String {
    if display_width(value) <= width {
        return value.to_owned();
    }
    if width <= 1 {
        return "…".chars().take(width).collect();
    }
    let mut output = value.chars().take(width - 1).collect::<String>();
    output.push('…');
    output
}

fn fill_to_width(value: &str, width: usize, fill: char) -> String {
    let mut output = truncate(value, width);
    output.extend(std::iter::repeat_n(
        fill,
        width.saturating_sub(display_width(&output)),
    ));
    output
}

fn write_rule(writer: &mut dyn Write, widths: &[usize]) -> io::Result<()> {
    writer.write_all(b"+")?;
    for width in widths {
        write!(writer, "-{}-+", "-".repeat(*width))?;
    }
    writer.write_all(b"\n")
}

fn write_horizontal_row(
    writer: &mut dyn Write,
    cells: &[&str],
    widths: &[usize],
) -> io::Result<()> {
    writer.write_all(b"|")?;
    for (index, width) in widths.iter().enumerate() {
        let value = truncate(cells.get(index).copied().unwrap_or("NULL"), *width);
        let padding = width.saturating_sub(display_width(&value));
        write!(writer, " {value}{} |", " ".repeat(padding))?;
    }
    writer.write_all(b"\n")
}

#[cfg(test)]
mod tests {
    use super::super::columns::rename_columns;
    use super::*;
    use crate::athena::api::ResultColumn;

    fn columns(names: &[&str]) -> Vec<Column> {
        rename_columns(
            &names
                .iter()
                .map(|name| ResultColumn {
                    name: (*name).to_owned(),
                    data_type: "varchar".to_owned(),
                })
                .collect::<Vec<_>>(),
        )
        .0
    }

    #[test]
    fn horizontal_table_truncates_to_its_fixed_width() {
        let columns = columns(&["name", "note"]);
        let mut output = Vec::new();
        let mut table = Table::begin(&mut output, &columns, Some(40)).expect("table");
        table
            .row(
                &mut output,
                &columns,
                &[
                    Some("honk".to_owned()),
                    Some("a very long value that cannot fit".to_owned()),
                ],
            )
            .expect("row");
        table.finish(&mut output).expect("finish");
        let rendered = String::from_utf8(output).expect("UTF-8");
        assert!(rendered.contains("a very long val…"));
        assert!(rendered.lines().all(|line| line.chars().count() <= 40));
    }

    #[test]
    fn wide_schema_uses_expanded_records() {
        let columns = columns(&["one", "two", "three", "four"]);
        let mut output = Vec::new();
        let mut table = Table::begin(&mut output, &columns, Some(40)).expect("table");
        table
            .row(
                &mut output,
                &columns,
                &[
                    Some("1".into()),
                    Some("2".into()),
                    Some("3".into()),
                    Some("4".into()),
                ],
            )
            .expect("row");
        table.finish(&mut output).expect("finish");
        let rendered = String::from_utf8(output).expect("UTF-8");
        assert!(rendered.contains("-[ Row 1 ]"));
        assert!(rendered.contains("three | 3"));
    }
}
