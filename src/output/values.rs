use std::io::{self, Write};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ValueKind {
    Boolean,
    Integer,
    Float,
    Text,
}

impl ValueKind {
    pub(super) fn parse(data_type: &str) -> Self {
        let head = data_type
            .trim()
            .split(['(', '<'])
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match head.as_str() {
            "boolean" => Self::Boolean,
            "tinyint" | "smallint" | "integer" | "int" | "bigint" => Self::Integer,
            "float" | "real" | "double" => Self::Float,
            _ => Self::Text,
        }
    }

    pub(super) fn convert(self, raw: Option<&str>) -> Value<'_> {
        let Some(raw) = raw else {
            return Value::Null;
        };
        match self {
            Self::Boolean => match raw {
                "true" => Value::Boolean(true),
                "false" => Value::Boolean(false),
                _ => Value::Text(raw),
            },
            Self::Integer => raw.parse().map_or(Value::Text(raw), Value::Integer),
            Self::Float => raw
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .map_or(Value::Text(raw), Value::Float),
            Self::Text => Value::Text(raw),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Value<'a> {
    Null,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    Text(&'a str),
}

impl Value<'_> {
    pub(crate) fn write_json(self, writer: &mut dyn Write) -> io::Result<()> {
        match self {
            Self::Null => writer.write_all(b"null"),
            Self::Boolean(value) => write!(writer, "{value}"),
            Self::Integer(value) => write!(writer, "{value}"),
            Self::Float(value) => write!(writer, "{value}"),
            Self::Text(value) => serde_json::to_writer(writer, value).map_err(io::Error::other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversion_uses_declared_types_and_falls_back_to_text() {
        assert_eq!(
            ValueKind::Boolean.convert(Some("true")),
            Value::Boolean(true)
        );
        assert_eq!(ValueKind::Integer.convert(Some("42")), Value::Integer(42));
        assert_eq!(ValueKind::Float.convert(Some("3.5")), Value::Float(3.5));
        assert_eq!(ValueKind::Integer.convert(Some("odd")), Value::Text("odd"));
        assert_eq!(ValueKind::Text.convert(Some("42")), Value::Text("42"));
        assert_eq!(ValueKind::Text.convert(None), Value::Null);
        assert_eq!(ValueKind::parse("decimal(38, 9)"), ValueKind::Text);
        assert_eq!(ValueKind::parse("array(varchar)"), ValueKind::Text);
    }
}
