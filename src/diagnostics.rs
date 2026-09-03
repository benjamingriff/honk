const PANIC_DIAGNOSTIC: &str = "error: Honk encountered an unexpected internal error";

/// Installs a panic hook that never prints panic payloads or captured state.
///
/// Panic payloads can contain SQL, credentials, or provider diagnostics. The
/// command emits one fixed message and discards write failures.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|_| {
        let stderr = std::io::stderr();
        let _ = write_panic_diagnostic(&mut stderr.lock());
    }));
}

fn write_panic_diagnostic(writer: &mut dyn std::io::Write) -> std::io::Result<()> {
    writeln!(writer, "{PANIC_DIAGNOSTIC}")
}

pub(crate) fn terminal_text(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_control() {
            output.extend(character.escape_unicode());
        } else {
            output.push(character);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_diagnostic_is_fixed_and_contains_no_runtime_state() {
        let mut output = Vec::new();
        write_panic_diagnostic(&mut output).expect("diagnostic");
        assert_eq!(
            String::from_utf8(output).expect("UTF-8"),
            "error: Honk encountered an unexpected internal error\n"
        );
    }

    #[test]
    fn terminal_text_escapes_control_characters() {
        assert_eq!(terminal_text("name\n\u{1b}[31m"), "name\\u{a}\\u{1b}[31m");
    }
}
