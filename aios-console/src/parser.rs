use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Lexical error with a character offset into the original input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// Human-readable description of the problem.
    pub message: String,
    /// Character offset where the problem was detected.
    pub offset: usize,
}

impl ParseError {
    fn new(message: &str, offset: usize) -> Self {
        Self {
            message: message.to_string(),
            offset,
        }
    }
}

/// One token produced by the tokenizer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// Token text with quotes and escapes removed.
    pub text: String,
    /// Whether the token was quoted and therefore must stay a single argument.
    pub quoted: bool,
}

/// Splits a command line into tokens, honouring quotes, escapes and `#` comments.
pub fn tokenize(input: &str) -> Result<Vec<Token>, ParseError> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0usize;

    while index < chars.len() {
        if chars[index].is_whitespace() {
            index += 1;
            continue;
        }
        if chars[index] == '#' {
            break;
        }

        let mut text = String::new();
        let mut quoted = false;
        let mut consumed = false;

        loop {
            if index >= chars.len() {
                break;
            }
            let current = chars[index];
            if current == '\\' {
                if index + 1 >= chars.len() {
                    return Err(ParseError::new("trailing escape character", index));
                }
                text.push(chars[index + 1]);
                index += 2;
                consumed = true;
                continue;
            }
            if current == '"' || current == '\'' {
                let quote = current;
                quoted = true;
                consumed = true;
                index += 1;
                let mut closed = false;
                while index < chars.len() {
                    if chars[index] == '\\' {
                        if index + 1 >= chars.len() {
                            return Err(ParseError::new("trailing escape character", index));
                        }
                        text.push(chars[index + 1]);
                        index += 2;
                        continue;
                    }
                    if chars[index] == quote {
                        index += 1;
                        closed = true;
                        break;
                    }
                    text.push(chars[index]);
                    index += 1;
                }
                if !closed {
                    return Err(ParseError::new("unclosed quote", index));
                }
                continue;
            }
            if current.is_whitespace() || current == '#' {
                break;
            }
            text.push(current);
            index += 1;
            consumed = true;
        }

        if consumed {
            tokens.push(Token { text, quoted });
        }
    }

    Ok(tokens)
}

/// A parsed command line before registry resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedLine {
    /// Original input, including comments.
    pub raw: String,
    /// Command head in lowercase; argument case is never changed.
    pub name: String,
    /// All tokens after the command head with their original case.
    pub tokens: Vec<String>,
    /// Positional arguments after flags are removed.
    pub positionals: Vec<String>,
    /// Parsed `--key=value`, `--key value` and `-k value` flags.
    pub flags: BTreeMap<String, Option<String>>,
}

impl ParsedLine {
    /// Returns whether no command was entered.
    pub fn is_empty(&self) -> bool {
        self.name.is_empty()
    }
}

/// Parses one line without consulting the command registry.
pub fn parse_line(input: &str) -> Result<ParsedLine, ParseError> {
    let tokens = tokenize(input)?;
    let mut result = ParsedLine {
        raw: input.to_string(),
        name: String::new(),
        tokens: Vec::new(),
        positionals: Vec::new(),
        flags: BTreeMap::new(),
    };
    let Some(head) = tokens.first() else {
        return Ok(result);
    };

    result.name = head.text.to_lowercase();
    rest_tokens(&tokens[1..], &mut result);
    Ok(result)
}

fn rest_tokens(tokens: &[Token], result: &mut ParsedLine) {
    let mut index = 0usize;
    while index < tokens.len() {
        let text = tokens[index].text.clone();
        result.tokens.push(text.clone());

        if text == "--" {
            while index < tokens.len() {
                result.positionals.push(tokens[index].text.clone());
                index += 1;
            }
            return;
        }

        let stripped = text.strip_prefix("--").or_else(|| text.strip_prefix('-'));
        let Some(stripped) = stripped else {
            result.positionals.push(text);
            index += 1;
            continue;
        };
        if stripped.is_empty() {
            result.positionals.push(text);
            index += 1;
            continue;
        }

        let (key, inline_value) = match stripped.split_once('=') {
            Some((key, value)) => (key.to_string(), Some(value.to_string())),
            None => (stripped.to_string(), None),
        };

        if inline_value.is_some() {
            result.flags.insert(key, inline_value);
            index += 1;
        } else if index + 1 < tokens.len() && !tokens[index + 1].text.starts_with('-') {
            let value = tokens[index + 1].text.clone();
            result.tokens.push(value.clone());
            result.flags.insert(key, Some(value));
            index += 2;
        } else {
            result.flags.insert(key, None);
            index += 1;
        }
    }
}

/// Turns an alias target into the head and leading positional tokens to prepend.
pub(crate) fn split_alias_target(target: &str) -> Result<(String, Vec<String>), ParseError> {
    let parsed = parse_line(target)?;
    Ok((parsed.name, parsed.positionals))
}
