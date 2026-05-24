const RESET: &str = "\x1b[0m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";

pub fn red(text: impl AsRef<str>) -> String {
    paint(RED, text.as_ref())
}

pub fn yellow(text: impl AsRef<str>) -> String {
    paint(YELLOW, text.as_ref())
}

fn paint(color: &str, text: &str) -> String {
    format!("{color}{text}{RESET}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_warning_and_error_text() {
        assert_eq!(
            yellow("warning: check history"),
            "\x1b[33mwarning: check history\x1b[0m"
        );
        assert_eq!(red("error: failed"), "\x1b[31merror: failed\x1b[0m");
    }
}
