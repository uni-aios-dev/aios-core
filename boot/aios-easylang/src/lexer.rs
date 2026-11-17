//! The lexer: turns EasyLang source into positioned tokens.
//!
//! EasyLang is line-oriented: a newline ends a statement, `;` is an equivalent
//! separator, `#` starts a comment, and strings are the only place where a
//! non-ASCII byte is allowed. Every token carries its `line:column` so the
//! parser and the code generator can report an exact position.

use crate::error::Error;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// What a token is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Kind {
    /// An identifier, `[A-Za-z_][A-Za-z0-9_]*`.
    Ident(String),
    /// A decimal `i32` literal.
    Int(i32),
    /// A decoded string literal, escapes already applied.
    Str(String),
    /// `let`
    Let,
    /// `if`
    If,
    /// `else`
    Else,
    /// `while`
    While,
    /// `log`
    Log,
    /// `exit`
    Exit,
    /// `yield`
    Yield,
    /// `and`
    And,
    /// `or`
    Or,
    /// `not`
    Not,
    /// `true`, compiled as `1`.
    True,
    /// `false`, compiled as `0`.
    False,
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `*`
    Star,
    /// `/`
    Slash,
    /// `%`
    Percent,
    /// `=`
    Assign,
    /// `==`
    EqEq,
    /// `!=`
    NotEq,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// A statement separator: a newline or `;`.
    Newline,
    /// End of input.
    Eof,
}

/// A token and where it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Token {
    /// The token itself.
    pub kind: Kind,
    /// `1`-based line.
    pub line: usize,
    /// `1`-based byte column.
    pub column: usize,
}

/// Splits `source` into tokens, ending with [`Kind::Eof`].
pub(crate) fn tokenize(source: &str) -> Result<Vec<Token>, Error> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    let mut line = 1;
    let mut column = 1;
    while index < bytes.len() {
        let byte = bytes[index];
        let (token_line, token_column) = (line, column);
        match byte {
            b' ' | b'\t' | b'\r' => {
                index += 1;
                column += 1;
            }
            b'\n' => {
                tokens.push(Token {
                    kind: Kind::Newline,
                    line,
                    column,
                });
                index += 1;
                line += 1;
                column = 1;
            }
            b';' => {
                tokens.push(Token {
                    kind: Kind::Newline,
                    line,
                    column,
                });
                index += 1;
                column += 1;
            }
            b'#' => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'0'..=b'9' => {
                let start = index;
                let mut value: i64 = 0;
                while index < bytes.len() && bytes[index].is_ascii_digit() {
                    value = value * 10 + i64::from(bytes[index] - b'0');
                    if value > i64::from(i32::MAX) {
                        return Err(Error::new(line, column, "integer literal is too large"));
                    }
                    index += 1;
                    column += 1;
                }
                if index < bytes.len() && is_ident_byte(bytes[index]) {
                    return Err(Error::new(
                        line,
                        column,
                        "a number cannot be followed by a letter",
                    ));
                }
                debug_assert!(index > start);
                tokens.push(Token {
                    kind: Kind::Int(value as i32),
                    line: token_line,
                    column: token_column,
                });
            }
            b'"' => {
                index += 1;
                column += 1;
                let mut text = String::new();
                loop {
                    if index >= bytes.len() || bytes[index] == b'\n' {
                        return Err(Error::new(token_line, token_column, "unterminated string"));
                    }
                    let current = bytes[index];
                    if current == b'"' {
                        index += 1;
                        column += 1;
                        break;
                    }
                    if current == b'\\' {
                        index += 1;
                        column += 1;
                        let Some(escape) = bytes.get(index).copied() else {
                            return Err(Error::new(
                                token_line,
                                token_column,
                                "unterminated string",
                            ));
                        };
                        let decoded = match escape {
                            b'n' => '\n',
                            b't' => '\t',
                            b'r' => '\r',
                            b'\\' => '\\',
                            b'"' => '"',
                            b'0' => '\0',
                            _ => {
                                return Err(Error::new(
                                    line,
                                    column,
                                    format!("unknown escape '\\{}'", escape as char),
                                ))
                            }
                        };
                        text.push(decoded);
                        index += 1;
                        column += 1;
                        continue;
                    }
                    // The source is `&str`, so a UTF-8 sequence is copied as is:
                    // the literal is a byte string inside the block.
                    let end = if current < 0x80 {
                        index + 1
                    } else {
                        let width = utf8_width(current);
                        let slice = source
                            .get(index..index + width)
                            .ok_or_else(|| Error::new(line, column, "invalid UTF-8 in a string"))?;
                        text.push_str(slice);
                        index += width;
                        column += width;
                        continue;
                    };
                    text.push(current as char);
                    index = end;
                    column += 1;
                }
                tokens.push(Token {
                    kind: Kind::Str(text),
                    line: token_line,
                    column: token_column,
                });
            }
            b'A'..=b'Z' | b'a'..=b'z' | b'_' => {
                let start = index;
                while index < bytes.len() && is_ident_byte(bytes[index]) {
                    index += 1;
                    column += 1;
                }
                let word = &source[start..index];
                let kind = match word {
                    "let" => Kind::Let,
                    "if" => Kind::If,
                    "else" => Kind::Else,
                    "while" => Kind::While,
                    "log" => Kind::Log,
                    "exit" => Kind::Exit,
                    "yield" => Kind::Yield,
                    "and" => Kind::And,
                    "or" => Kind::Or,
                    "not" => Kind::Not,
                    "true" => Kind::True,
                    "false" => Kind::False,
                    _ => Kind::Ident(String::from(word)),
                };
                tokens.push(Token {
                    kind,
                    line: token_line,
                    column: token_column,
                });
            }
            b'+' => single(
                &mut tokens,
                &mut index,
                &mut column,
                Kind::Plus,
                line,
                token_column,
            ),
            b'-' => single(
                &mut tokens,
                &mut index,
                &mut column,
                Kind::Minus,
                line,
                token_column,
            ),
            b'*' => single(
                &mut tokens,
                &mut index,
                &mut column,
                Kind::Star,
                line,
                token_column,
            ),
            b'/' => single(
                &mut tokens,
                &mut index,
                &mut column,
                Kind::Slash,
                line,
                token_column,
            ),
            b'%' => single(
                &mut tokens,
                &mut index,
                &mut column,
                Kind::Percent,
                line,
                token_column,
            ),
            b'(' => single(
                &mut tokens,
                &mut index,
                &mut column,
                Kind::LParen,
                line,
                token_column,
            ),
            b')' => single(
                &mut tokens,
                &mut index,
                &mut column,
                Kind::RParen,
                line,
                token_column,
            ),
            b'{' => single(
                &mut tokens,
                &mut index,
                &mut column,
                Kind::LBrace,
                line,
                token_column,
            ),
            b'}' => single(
                &mut tokens,
                &mut index,
                &mut column,
                Kind::RBrace,
                line,
                token_column,
            ),
            b'=' => {
                index += 1;
                column += 1;
                if bytes.get(index) == Some(&b'=') {
                    index += 1;
                    column += 1;
                    push(&mut tokens, Kind::EqEq, line, token_column);
                } else {
                    push(&mut tokens, Kind::Assign, line, token_column);
                }
            }
            b'!' => {
                index += 1;
                column += 1;
                if bytes.get(index) == Some(&b'=') {
                    index += 1;
                    column += 1;
                    push(&mut tokens, Kind::NotEq, line, token_column);
                } else {
                    return Err(Error::new(line, token_column, "expected '!='"));
                }
            }
            b'<' => {
                index += 1;
                column += 1;
                if bytes.get(index) == Some(&b'=') {
                    index += 1;
                    column += 1;
                    push(&mut tokens, Kind::Le, line, token_column);
                } else {
                    push(&mut tokens, Kind::Lt, line, token_column);
                }
            }
            b'>' => {
                index += 1;
                column += 1;
                if bytes.get(index) == Some(&b'=') {
                    index += 1;
                    column += 1;
                    push(&mut tokens, Kind::Ge, line, token_column);
                } else {
                    push(&mut tokens, Kind::Gt, line, token_column);
                }
            }
            _ => {
                return Err(Error::new(
                    line,
                    column,
                    if byte.is_ascii() {
                        format!("unexpected character '{}'", byte as char)
                    } else {
                        String::from("unexpected non-ASCII character outside a string")
                    },
                ))
            }
        }
    }
    tokens.push(Token {
        kind: Kind::Eof,
        line,
        column,
    });
    Ok(tokens)
}

fn push(tokens: &mut Vec<Token>, kind: Kind, line: usize, column: usize) {
    tokens.push(Token { kind, line, column });
}

fn single(
    tokens: &mut Vec<Token>,
    index: &mut usize,
    column: &mut usize,
    kind: Kind,
    line: usize,
    at: usize,
) {
    *index += 1;
    *column += 1;
    push(tokens, kind, line, at);
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn utf8_width(lead: u8) -> usize {
    match lead {
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn kinds(source: &str) -> Vec<Kind> {
        tokenize(source)
            .unwrap()
            .into_iter()
            .map(|token| token.kind)
            .collect()
    }

    #[test]
    fn splits_a_program() {
        assert_eq!(
            kinds("let x = 1\n"),
            vec![
                Kind::Let,
                Kind::Ident(String::from("x")),
                Kind::Assign,
                Kind::Int(1),
                Kind::Newline,
                Kind::Eof
            ]
        );
    }

    #[test]
    fn semicolons_are_newlines_and_comments_are_skipped() {
        assert_eq!(
            kinds("log \"a\"; # comment\nlog \"b\""),
            vec![
                Kind::Log,
                Kind::Str(String::from("a")),
                Kind::Newline,
                Kind::Newline,
                Kind::Log,
                Kind::Str(String::from("b")),
                Kind::Eof
            ]
        );
    }

    #[test]
    fn keywords_and_operators_are_distinct() {
        assert_eq!(
            kinds("if a <= b and not c != 2"),
            vec![
                Kind::If,
                Kind::Ident(String::from("a")),
                Kind::Le,
                Kind::Ident(String::from("b")),
                Kind::And,
                Kind::Not,
                Kind::Ident(String::from("c")),
                Kind::NotEq,
                Kind::Int(2),
                Kind::Eof
            ]
        );
    }

    #[test]
    fn strings_decode_escapes() {
        assert_eq!(
            kinds("log \"a\\tb\\n\\\"q\\\"\\\\\""),
            vec![
                Kind::Log,
                Kind::Str(String::from("a\tb\n\"q\"\\")),
                Kind::Eof
            ]
        );
    }

    #[test]
    fn positions_are_one_based() {
        let tokens = tokenize("let\n  x = 1").unwrap();
        assert_eq!((tokens[0].line, tokens[0].column), (1, 1));
        assert_eq!((tokens[1].line, tokens[1].column), (1, 4));
        assert_eq!((tokens[2].line, tokens[2].column), (2, 3));
    }

    #[test]
    fn bad_input_is_reported() {
        assert!(tokenize("\"open")
            .unwrap_err()
            .message
            .contains("unterminated"));
        assert!(tokenize("\"a\\q\"").unwrap_err().message.contains("escape"));
        assert!(tokenize("1x").unwrap_err().message.contains("letter"));
        assert!(tokenize("99999999999")
            .unwrap_err()
            .message
            .contains("too large"));
        assert!(tokenize("`").unwrap_err().message.contains("unexpected"));
        assert!(tokenize("!x").unwrap_err().message.contains("!="));
    }
}
