//! Turns query text into tokens.

use crate::QueryError;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Token {
    /// A name or a keyword (keywords are matched without case by the parser).
    Ident(String),
    /// `$name`
    Param(String),
    /// `@name`: a pseudo-property such as `@id` or `@text`.
    Meta(String),
    Str(String),
    Int(i64),
    Float(f64),
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Colon,
    Dot,
    DotDot,
    Pipe,
    Star,
    Plus,
    PlusEq,
    Minus,
    Slash,
    Percent,
    Eq,
    Neq,
    Lt,
    Le,
    Gt,
    Ge,
    Semicolon,
    End,
}

impl Token {
    /// How the token reads in error messages.
    pub(crate) fn describe(&self) -> String {
        match self {
            Token::Ident(name) => format!("`{name}`"),
            Token::Param(name) => format!("`${name}`"),
            Token::Meta(name) => format!("`@{name}`"),
            Token::Str(_) => "a string".into(),
            Token::Int(_) | Token::Float(_) => "a number".into(),
            Token::End => "the end of the query".into(),
            other => format!("`{}`", other.symbol()),
        }
    }

    fn symbol(&self) -> &'static str {
        match self {
            Token::LParen => "(",
            Token::RParen => ")",
            Token::LBracket => "[",
            Token::RBracket => "]",
            Token::LBrace => "{",
            Token::RBrace => "}",
            Token::Comma => ",",
            Token::Colon => ":",
            Token::Dot => ".",
            Token::DotDot => "..",
            Token::Pipe => "|",
            Token::Star => "*",
            Token::Plus => "+",
            Token::PlusEq => "+=",
            Token::Minus => "-",
            Token::Slash => "/",
            Token::Percent => "%",
            Token::Eq => "=",
            Token::Neq => "<>",
            Token::Lt => "<",
            Token::Le => "<=",
            Token::Gt => ">",
            Token::Ge => ">=",
            Token::Semicolon => ";",
            _ => "?",
        }
    }
}

/// A token and the byte range it came from.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Spanned {
    pub(crate) token: Token,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

pub(crate) fn lex(source: &str) -> Result<Vec<Spanned>, QueryError> {
    let chars: Vec<(usize, char)> = source.char_indices().collect();
    let at = |i: usize| chars.get(i).map(|&(_, c)| c);
    let offset = |i: usize| chars.get(i).map_or(source.len(), |&(o, _)| o);
    let mut tokens = Vec::new();
    let mut i = 0;
    while let Some(c) = at(i) {
        let start = offset(i);
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '/' && at(i + 1) == Some('/') {
            while at(i).is_some_and(|c| c != '\n') {
                i += 1;
            }
            continue;
        }
        let single = |token: Token, i: &mut usize| {
            *i += 1;
            token
        };
        let token = match c {
            '(' => single(Token::LParen, &mut i),
            ')' => single(Token::RParen, &mut i),
            '[' => single(Token::LBracket, &mut i),
            ']' => single(Token::RBracket, &mut i),
            '{' => single(Token::LBrace, &mut i),
            '}' => single(Token::RBrace, &mut i),
            ',' => single(Token::Comma, &mut i),
            ':' => single(Token::Colon, &mut i),
            '|' => single(Token::Pipe, &mut i),
            '*' => single(Token::Star, &mut i),
            '-' => single(Token::Minus, &mut i),
            '/' => single(Token::Slash, &mut i),
            '%' => single(Token::Percent, &mut i),
            '=' => single(Token::Eq, &mut i),
            ';' => single(Token::Semicolon, &mut i),
            '.' if at(i + 1) == Some('.') => {
                i += 2;
                Token::DotDot
            }
            '.' => single(Token::Dot, &mut i),
            '+' if at(i + 1) == Some('=') => {
                i += 2;
                Token::PlusEq
            }
            '+' => single(Token::Plus, &mut i),
            '!' if at(i + 1) == Some('=') => {
                i += 2;
                Token::Neq
            }
            '<' if at(i + 1) == Some('>') => {
                i += 2;
                Token::Neq
            }
            '<' if at(i + 1) == Some('=') => {
                i += 2;
                Token::Le
            }
            '<' => single(Token::Lt, &mut i),
            '>' if at(i + 1) == Some('=') => {
                i += 2;
                Token::Ge
            }
            '>' => single(Token::Gt, &mut i),
            '\'' | '"' => {
                let quote = c;
                let mut text = String::new();
                i += 1;
                loop {
                    match at(i) {
                        None => return Err(QueryError::at(source, start, "unterminated string")),
                        Some(c) if c == quote => {
                            i += 1;
                            break;
                        }
                        Some('\\') => {
                            let escaped = at(i + 1).ok_or_else(|| {
                                QueryError::at(source, start, "unterminated string")
                            })?;
                            i += 2;
                            match escaped {
                                'n' => text.push('\n'),
                                't' => text.push('\t'),
                                'r' => text.push('\r'),
                                'u' => {
                                    let hex: String = (0..4).filter_map(|k| at(i + k)).collect();
                                    let code = u32::from_str_radix(&hex, 16)
                                        .ok()
                                        .and_then(char::from_u32)
                                        .ok_or_else(|| {
                                            QueryError::at(source, offset(i), "invalid \\u escape")
                                        })?;
                                    text.push(code);
                                    i += 4;
                                }
                                other => text.push(other),
                            }
                        }
                        Some(c) => {
                            text.push(c);
                            i += 1;
                        }
                    }
                }
                Token::Str(text)
            }
            '`' => {
                let mut name = String::new();
                i += 1;
                loop {
                    match at(i) {
                        None => return Err(QueryError::at(source, start, "unterminated `name`")),
                        Some('`') => {
                            i += 1;
                            break;
                        }
                        Some(c) => {
                            name.push(c);
                            i += 1;
                        }
                    }
                }
                Token::Ident(name)
            }
            '$' | '@' => {
                i += 1;
                let mut name = String::new();
                while let Some(c) = at(i).filter(|c| c.is_alphanumeric() || *c == '_') {
                    name.push(c);
                    i += 1;
                }
                if name.is_empty() {
                    return Err(QueryError::at(
                        source,
                        start,
                        format!("expected a name after `{c}`"),
                    ));
                }
                if c == '$' {
                    Token::Param(name)
                } else {
                    Token::Meta(name)
                }
            }
            c if c.is_ascii_digit() => {
                let mut text = String::new();
                while let Some(c) = at(i).filter(char::is_ascii_digit) {
                    text.push(c);
                    i += 1;
                }
                // A fraction needs a digit after the dot, so `1..3` stays a range.
                let is_float = at(i) == Some('.') && at(i + 1).is_some_and(|c| c.is_ascii_digit());
                if is_float {
                    text.push('.');
                    i += 1;
                    while let Some(c) = at(i).filter(char::is_ascii_digit) {
                        text.push(c);
                        i += 1;
                    }
                }
                if matches!(at(i), Some('e' | 'E'))
                    && (at(i + 1).is_some_and(|c| c.is_ascii_digit())
                        || (matches!(at(i + 1), Some('+' | '-'))
                            && at(i + 2).is_some_and(|c| c.is_ascii_digit())))
                {
                    text.push('e');
                    i += 1;
                    if let Some(sign) = at(i).filter(|c| *c == '+' || *c == '-') {
                        text.push(sign);
                        i += 1;
                    }
                    while let Some(c) = at(i).filter(char::is_ascii_digit) {
                        text.push(c);
                        i += 1;
                    }
                    let value = text
                        .parse()
                        .map_err(|_| QueryError::at(source, start, "invalid number"))?;
                    Token::Float(value)
                } else if is_float {
                    let value = text
                        .parse()
                        .map_err(|_| QueryError::at(source, start, "invalid number"))?;
                    Token::Float(value)
                } else {
                    let value = text
                        .parse()
                        .map_err(|_| QueryError::at(source, start, "integer too large"))?;
                    Token::Int(value)
                }
            }
            c if c.is_alphabetic() || c == '_' => {
                let mut name = String::new();
                while let Some(c) = at(i).filter(|c| c.is_alphanumeric() || *c == '_') {
                    name.push(c);
                    i += 1;
                }
                Token::Ident(name)
            }
            other => {
                return Err(QueryError::at(
                    source,
                    start,
                    format!("unexpected character {other:?}"),
                ));
            }
        };
        tokens.push(Spanned {
            token,
            start,
            end: offset(i),
        });
    }
    tokens.push(Spanned {
        token: Token::End,
        start: source.len(),
        end: source.len(),
    });
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(source: &str) -> Vec<Token> {
        lex(source).unwrap().into_iter().map(|t| t.token).collect()
    }

    #[test]
    fn lexes_patterns_numbers_and_strings() {
        assert_eq!(
            tokens("(a:Person)-[:knows*1..3]->(b) // comment"),
            vec![
                Token::LParen,
                Token::Ident("a".into()),
                Token::Colon,
                Token::Ident("Person".into()),
                Token::RParen,
                Token::Minus,
                Token::LBracket,
                Token::Colon,
                Token::Ident("knows".into()),
                Token::Star,
                Token::Int(1),
                Token::DotDot,
                Token::Int(3),
                Token::RBracket,
                Token::Minus,
                Token::Gt,
                Token::LParen,
                Token::Ident("b".into()),
                Token::RParen,
                Token::End,
            ]
        );
        assert_eq!(
            tokens(r#"1.5 2e3 'it\'s' "á" `odd name` $p @text <> != <= >= +="#),
            vec![
                Token::Float(1.5),
                Token::Float(2000.0),
                Token::Str("it's".into()),
                Token::Str("á".into()),
                Token::Ident("odd name".into()),
                Token::Param("p".into()),
                Token::Meta("text".into()),
                Token::Neq,
                Token::Neq,
                Token::Le,
                Token::Ge,
                Token::PlusEq,
                Token::End,
            ]
        );
    }

    #[test]
    fn reports_where_lexing_failed() {
        let error = lex("MATCH (n)\nRETURN 'oops").unwrap_err();
        assert_eq!(error.to_string(), "unterminated string (line 2, column 8)");
        assert!(lex("RETURN #").is_err());
    }
}
