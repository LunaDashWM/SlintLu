//! Command lines the user writes (custom menu entries, …): split into a program and its
//! arguments, never evaluated by a shell.

/// Why a command line cannot be run.
#[derive(Debug, PartialEq, Eq)]
pub enum CommandLineError {
    /// Nothing but whitespace.
    Empty,
    /// A `'` or `"` is not closed.
    UnclosedQuote,
}

impl std::fmt::Display for CommandLineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("the command is empty"),
            Self::UnclosedQuote => f.write_str("a quote is not closed"),
        }
    }
}

impl std::error::Error for CommandLineError {}

/// Splits `line` into words like a POSIX shell does, without evaluating anything: whitespace
/// separates words, `'…'` quotes literally, `"…"` quotes with `\` escaping `"`, `\`, `$` and
/// `` ` ``, and outside quotes `\` keeps the next character. Variables, globs, pipes,
/// redirections and `;` mean nothing: they reach the program as text. An unquoted `~` that
/// starts a word, alone or before `/`, is `home` (when it is not empty).
pub fn split_command_line(line: &str, home: &str) -> Result<Vec<String>, CommandLineError> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return Err(CommandLineError::UnclosedQuote),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\' | '$' | '`')) => word.push(c),
                            Some('\n') => {}
                            Some(c) => {
                                word.push('\\');
                                word.push(c);
                            }
                            None => return Err(CommandLineError::UnclosedQuote),
                        },
                        Some(c) => word.push(c),
                        None => return Err(CommandLineError::UnclosedQuote),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some('\n') => {}
                    Some(c) => word.push(c),
                    None => word.push('\\'),
                }
            }
            '~' if !in_word
                && !home.is_empty()
                && chars.peek().is_none_or(|n| *n == '/' || n.is_whitespace()) =>
            {
                in_word = true;
                word.push_str(home);
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    if words.is_empty() {
        return Err(CommandLineError::Empty);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(line: &str) -> Vec<String> {
        split_command_line(line, "/home/me").unwrap()
    }

    #[test]
    fn splits_on_whitespace() {
        assert_eq!(split("  kitty   -e  htop "), vec!["kitty", "-e", "htop"]);
    }

    #[test]
    fn quotes_group_words() {
        assert_eq!(
            split(r#"notify-send "Hello world" 'it''s'"#),
            vec!["notify-send", "Hello world", "its"]
        );
        assert_eq!(split(r#"echo "a \"b\" \$c \d""#), vec!["echo", r#"a "b" $c \d"#]);
        assert_eq!(split(r"echo a\ b \'"), vec!["echo", "a b", "'"]);
        assert_eq!(split(r#"echo "" ''"#), vec!["echo", "", ""]);
    }

    #[test]
    fn nothing_is_evaluated() {
        assert_eq!(
            split("echo $HOME *.png | wc; rm -rf / && `id` > out"),
            vec!["echo", "$HOME", "*.png", "|", "wc;", "rm", "-rf", "/", "&&", "`id`", ">", "out"]
        );
    }

    #[test]
    fn tilde_starts_a_path() {
        assert_eq!(
            split("~/bin/tool ~ a~ '~/x' ~user"),
            vec!["/home/me/bin/tool", "/home/me", "a~", "~/x", "~user"]
        );
        assert_eq!(split_command_line("~/x", "").unwrap(), vec!["~/x"]);
    }

    #[test]
    fn rejects_empty_and_unclosed() {
        assert_eq!(split_command_line("   ", "/h"), Err(CommandLineError::Empty));
        assert_eq!(split_command_line("echo 'oops", "/h"), Err(CommandLineError::UnclosedQuote));
        assert_eq!(split_command_line("echo \"oops\\", "/h"), Err(CommandLineError::UnclosedQuote));
    }
}
