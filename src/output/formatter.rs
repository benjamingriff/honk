use std::io::{self, Write};

use crate::cli::OutputFormat;

use super::table::Table;
use super::{Column, Value};

pub(crate) struct Formatter<'a> {
    writer: &'a mut dyn Write,
    columns: &'a [Column],
    kind: Kind,
    rows: usize,
}

enum Kind {
    Table(Table),
    Csv,
    Tsv,
    Json,
    JsonLines,
    Markdown,
}

impl<'a> Formatter<'a> {
    pub(crate) fn begin(
        writer: &'a mut dyn Write,
        format: OutputFormat,
        columns: &'a [Column],
        table_width: Option<usize>,
    ) -> io::Result<Self> {
        let names = columns
            .iter()
            .map(|column| column.name.as_str())
            .collect::<Vec<_>>();
        let kind = match format {
            OutputFormat::Table => Kind::Table(Table::begin(writer, columns, table_width)?),
            OutputFormat::Csv => {
                write_delimited(writer, b',', names.iter().copied())?;
                Kind::Csv
            }
            OutputFormat::Tsv => {
                write_delimited(writer, b'\t', names.iter().copied())?;
                Kind::Tsv
            }
            OutputFormat::Json => {
                writer.write_all(b"[")?;
                Kind::Json
            }
            OutputFormat::Jsonl => Kind::JsonLines,
            OutputFormat::Markdown => {
                write_markdown_row(writer, names.iter().copied())?;
                writer.write_all(b"|")?;
                for _ in columns {
                    writer.write_all(b" --- |")?;
                }
                writer.write_all(b"\n")?;
                Kind::Markdown
            }
        };
        Ok(Self {
            writer,
            columns,
            kind,
            rows: 0,
        })
    }

    pub(crate) fn row(&mut self, raw: &[Option<String>]) -> io::Result<()> {
        let typed = self
            .columns
            .iter()
            .enumerate()
            .map(|(index, column)| column.value(raw.get(index).and_then(Option::as_deref)))
            .collect::<Vec<_>>();
        match &mut self.kind {
            Kind::Table(table) => table.row(self.writer, self.columns, raw)?,
            Kind::Csv => write_delimited(
                self.writer,
                b',',
                raw_text(raw, self.columns.len()).iter().map(String::as_str),
            )?,
            Kind::Tsv => write_delimited(
                self.writer,
                b'\t',
                raw_text(raw, self.columns.len()).iter().map(String::as_str),
            )?,
            Kind::Json => {
                if self.rows > 0 {
                    self.writer.write_all(b",")?;
                }
                write_json_object(self.writer, self.columns, &typed)?;
            }
            Kind::JsonLines => {
                write_json_object(self.writer, self.columns, &typed)?;
                self.writer.write_all(b"\n")?;
            }
            Kind::Markdown => write_markdown_row(
                self.writer,
                raw_text(raw, self.columns.len()).iter().map(String::as_str),
            )?,
        }
        self.rows += 1;
        Ok(())
    }

    pub(crate) fn finish(mut self) -> io::Result<usize> {
        match &mut self.kind {
            Kind::Table(table) => table.finish(self.writer)?,
            Kind::Json => self.writer.write_all(b"]\n")?,
            Kind::Csv | Kind::Tsv | Kind::JsonLines | Kind::Markdown => {}
        }
        self.writer.flush()?;
        Ok(self.rows)
    }
}

fn raw_text(raw: &[Option<String>], count: usize) -> Vec<String> {
    (0..count)
        .map(|index| {
            raw.get(index)
                .and_then(Option::as_deref)
                .unwrap_or("")
                .to_owned()
        })
        .collect()
}

fn write_delimited<'a>(
    writer: &mut dyn Write,
    delimiter: u8,
    fields: impl IntoIterator<Item = &'a str>,
) -> io::Result<()> {
    let mut buffer = Vec::new();
    {
        let mut csv = csv::WriterBuilder::new()
            .delimiter(delimiter)
            .terminator(csv::Terminator::Any(b'\n'))
            .from_writer(&mut buffer);
        csv.write_record(fields)?;
        csv.flush()?;
    }
    writer.write_all(&buffer)
}

fn write_json_object(
    writer: &mut dyn Write,
    columns: &[Column],
    values: &[Value<'_>],
) -> io::Result<()> {
    writer.write_all(b"{")?;
    for (index, column) in columns.iter().enumerate() {
        if index > 0 {
            writer.write_all(b",")?;
        }
        serde_json::to_writer(&mut *writer, &column.name).map_err(io::Error::other)?;
        writer.write_all(b":")?;
        values
            .get(index)
            .copied()
            .unwrap_or(Value::Null)
            .write_json(writer)?;
    }
    writer.write_all(b"}")
}

fn write_markdown_row<'a>(
    writer: &mut dyn Write,
    fields: impl IntoIterator<Item = &'a str>,
) -> io::Result<()> {
    writer.write_all(b"|")?;
    for field in fields {
        write!(writer, " {} |", markdown_escape(field))?;
    }
    writer.write_all(b"\n")
}

fn markdown_escape(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => output.push_str("\\\\"),
            '|' => output.push_str("\\|"),
            '\r' | '\n' => output.push_str("<br>"),
            value if value.is_control() => output.extend(value.escape_unicode()),
            value => output.push(value),
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;
    use crate::output::{ColumnSpec, rename_columns};

    fn columns() -> Vec<Column> {
        rename_columns(&[
            ColumnSpec {
                name: "text".into(),
                data_type: "varchar".into(),
            },
            ColumnSpec {
                name: "n".into(),
                data_type: "bigint".into(),
            },
            ColumnSpec {
                name: "ok".into(),
                data_type: "boolean".into(),
            },
            ColumnSpec {
                name: "missing".into(),
                data_type: "varchar".into(),
            },
        ])
        .0
    }

    fn render(format: OutputFormat) -> String {
        let columns = columns();
        let mut output = Vec::new();
        let mut formatter =
            Formatter::begin(&mut output, format, &columns, Some(120)).expect("begin");
        formatter
            .row(&[
                Some("a, \"quote\"\nline | snowman ☃".into()),
                Some("42".into()),
                Some("true".into()),
                None,
            ])
            .expect("row");
        formatter.finish().expect("finish");
        String::from_utf8(output).expect("UTF-8")
    }

    #[test]
    fn csv_and_tsv_quote_using_their_delimiters() {
        let csv = render(OutputFormat::Csv);
        assert!(csv.contains("\"a, \"\"quote\"\"\nline | snowman ☃\",42,true,"));
        let tsv = render(OutputFormat::Tsv);
        assert!(tsv.contains("\"a, \"\"quote\"\"\nline | snowman ☃\"\t42\ttrue\t"));
    }

    #[test]
    fn json_formats_use_declared_types_and_valid_escaping() {
        assert_eq!(
            render(OutputFormat::Json),
            "[{\"text\":\"a, \\\"quote\\\"\\nline | snowman ☃\",\"n\":42,\"ok\":true,\"missing\":null}]\n"
        );
        assert_eq!(
            render(OutputFormat::Jsonl),
            "{\"text\":\"a, \\\"quote\\\"\\nline | snowman ☃\",\"n\":42,\"ok\":true,\"missing\":null}\n"
        );
    }

    #[test]
    fn markdown_escapes_layout_characters() {
        let rendered = render(OutputFormat::Markdown);
        assert!(rendered.contains("a, \"quote\"<br>line \\| snowman ☃"));
    }

    #[test]
    fn empty_results_have_valid_format_specific_output() {
        let columns = columns();
        for (format, expected) in [
            (OutputFormat::Csv, "text,n,ok,missing\n"),
            (OutputFormat::Tsv, "text\tn\tok\tmissing\n"),
            (OutputFormat::Json, "[]\n"),
            (OutputFormat::Jsonl, ""),
            (
                OutputFormat::Markdown,
                "| text | n | ok | missing |\n| --- | --- | --- | --- |\n",
            ),
        ] {
            let mut output = Vec::new();
            Formatter::begin(&mut output, format, &columns, Some(120))
                .expect("begin")
                .finish()
                .expect("finish");
            assert_eq!(String::from_utf8(output).expect("UTF-8"), expected);
        }
    }

    struct LimitedWriter {
        bytes: Vec<u8>,
        remaining: usize,
    }

    impl Write for LimitedWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"));
            }
            let count = buffer.len().min(self.remaining);
            self.bytes.extend_from_slice(&buffer[..count]);
            self.remaining -= count;
            Ok(count)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn interrupted_json_lines_keeps_completed_lines_valid() {
        let columns = columns();
        let row = [
            Some("safe".into()),
            Some("1".into()),
            Some("true".into()),
            None,
        ];
        let mut one_line = Vec::new();
        let mut formatter =
            Formatter::begin(&mut one_line, OutputFormat::Jsonl, &columns, None).expect("begin");
        formatter.row(&row).expect("row");
        formatter.finish().expect("finish");

        let mut limited = LimitedWriter {
            bytes: Vec::new(),
            remaining: one_line.len(),
        };
        let mut formatter =
            Formatter::begin(&mut limited, OutputFormat::Jsonl, &columns, None).expect("begin");
        formatter.row(&row).expect("first row");
        assert!(formatter.row(&row).is_err());
        drop(formatter);

        let first = std::str::from_utf8(&limited.bytes)
            .expect("UTF-8")
            .trim_end();
        serde_json::from_str::<serde_json::Value>(first).expect("complete JSON line");
    }
}
