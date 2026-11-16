//! Tokens of a codemode script: the JavaScript lexical grammar, without
//! `BigInt`, private names and HTML comments.
//!
//! A template literal is lexed with its substitutions: each `${ ... }` becomes
//! the tokens inside it, so the parser reads them as an expression and their
//! lines are the script's. A `/` starts a regular expression where a value
//! cannot end, as engines decide it from the previous token.

use std::rc::Rc;

use crate::SyntaxError;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Num(f64),
    Str(Rc<str>),
    /// Cooked text before, between and after the substitutions; one more
    /// text than substitutions.
    Template(Vec<Rc<str>>, Vec<Vec<Token>>),
    Regex(Rc<str>, Rc<str>),
    /// Identifiers and keywords alike; the parser tells them apart.
    Ident(Rc<str>),
    Punct(&'static str),
    Eof,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub line: u32,
    pub col: u32,
    /// A line terminator came between this token and the one before it, for
    /// automatic semicolon insertion and the restricted productions.
    pub newline_before: bool,
}

/// Longest first, so `>>>=` is not read as `>>` and `>=`.
const PUNCTUATORS: [&str; 52] = [
    ">>>=", "...", "===", "!==", "**=", "<<=", ">>=", ">>>", "&&=", "||=", "??=", "=>", "==", "!=",
    "<=", ">=", "&&", "||", "??", "?.", "++", "--", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=",
    "**", "<<", ">>", "{", "}", "(", ")", "[", "]", ";", ",", "<", ">", "+", "-", "*", "/", "%",
    "&", "|", "^", "!",
];
const SINGLE: [&str; 6] = ["~", "?", ":", "=", ".", "@"];

/// Keywords after which a `/` starts a regular expression.
const REGEX_AFTER_WORD: [&str; 14] = [
    "return",
    "typeof",
    "instanceof",
    "in",
    "of",
    "new",
    "delete",
    "void",
    "throw",
    "case",
    "do",
    "else",
    "await",
    "yield",
];

struct Lexer {
    chars: Vec<char>,
    pos: usize,
    line: u32,
    col: u32,
}

/// The tokens of `source`, ending with [`Tok::Eof`].
pub fn tokenize(source: &str) -> Result<Vec<Token>, SyntaxError> {
    let mut lexer = Lexer {
        chars: source.chars().collect(),
        pos: 0,
        line: 1,
        col: 1,
    };
    let mut tokens = lexer.run(false)?;
    let line = lexer.line;
    tokens.push(Token {
        tok: Tok::Eof,
        line,
        col: lexer.col,
        newline_before: true,
    });
    Ok(tokens)
}

fn is_id_start(c: char) -> bool {
    c == '_' || c == '$' || c.is_alphabetic()
}

fn is_id_part(c: char) -> bool {
    c == '_' || c == '$' || c.is_alphanumeric() || c == '\u{200c}' || c == '\u{200d}'
}

fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

impl Lexer {
    fn peek(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.pos + ahead).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.chars.get(self.pos).copied()?;
        self.pos += 1;
        if c == '\n'
            || c == '\u{2028}'
            || c == '\u{2029}'
            || (c == '\r' && self.peek(0) != Some('\n'))
        {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    fn error(&self, message: impl Into<String>) -> SyntaxError {
        SyntaxError {
            message: message.into(),
            line: self.line,
            col: self.col,
        }
    }

    /// Skip blanks and comments; whether a line terminator was among them.
    fn skip_trivia(&mut self) -> Result<bool, SyntaxError> {
        let mut newline = false;
        loop {
            match self.peek(0) {
                Some(c) if is_line_terminator(c) => {
                    newline = true;
                    self.bump();
                }
                Some(c) if c.is_whitespace() || c == '\u{feff}' => {
                    self.bump();
                }
                Some('/') if self.peek(1) == Some('/') => {
                    while let Some(c) = self.peek(0) {
                        if is_line_terminator(c) {
                            break;
                        }
                        self.bump();
                    }
                }
                Some('/') if self.peek(1) == Some('*') => {
                    self.bump();
                    self.bump();
                    loop {
                        match self.bump() {
                            None => return Err(self.error("Unterminated comment")),
                            Some('*') if self.peek(0) == Some('/') => {
                                self.bump();
                                break;
                            }
                            Some(c) if is_line_terminator(c) => newline = true,
                            Some(_) => {}
                        }
                    }
                }
                _ => return Ok(newline),
            }
        }
    }

    /// Tokens up to the end of input, or, inside a template substitution, up
    /// to the `}` that closes it (consumed, not returned).
    fn run(&mut self, in_template: bool) -> Result<Vec<Token>, SyntaxError> {
        let mut tokens: Vec<Token> = Vec::new();
        let mut depth = 0_usize;
        loop {
            let newline_before = self.skip_trivia()?;
            let (line, col) = (self.line, self.col);
            let Some(c) = self.peek(0) else {
                if in_template {
                    return Err(self.error("Unterminated template literal"));
                }
                return Ok(tokens);
            };
            let tok = if in_template && c == '}' && depth == 0 {
                self.bump();
                return Ok(tokens);
            } else if is_id_start(c) || c == '\\' {
                Tok::Ident(self.identifier()?.into())
            } else if c.is_ascii_digit()
                || (c == '.' && self.peek(1).is_some_and(|d| d.is_ascii_digit()))
            {
                Tok::Num(self.number()?)
            } else if c == '"' || c == '\'' {
                Tok::Str(self.string(c)?.into())
            } else if c == '`' {
                self.template()?
            } else if c == '/' && regex_allowed(tokens.last()) {
                self.regex()?
            } else {
                let punct = self.punctuator()?;
                if in_template {
                    match punct {
                        "{" => depth += 1,
                        "}" => depth = depth.saturating_sub(1),
                        _ => {}
                    }
                }
                Tok::Punct(punct)
            };
            tokens.push(Token {
                tok,
                line,
                col,
                newline_before,
            });
        }
    }

    fn punctuator(&mut self) -> Result<&'static str, SyntaxError> {
        for punct in PUNCTUATORS.iter().chain(SINGLE.iter()) {
            let matches = punct
                .chars()
                .enumerate()
                .all(|(index, c)| self.peek(index) == Some(c));
            if !matches {
                continue;
            }
            // `a?.5:1` is a conditional, not an optional chain.
            if *punct == "?." && self.peek(2).is_some_and(|c| c.is_ascii_digit()) {
                continue;
            }
            for _ in 0..punct.chars().count() {
                self.bump();
            }
            return Ok(punct);
        }
        let c = self.peek(0).unwrap_or(' ');
        if c == '#' {
            return Err(self.error("Private names (#name) are not supported in codemode scripts"));
        }
        Err(self.error(format!("Invalid or unexpected token '{c}'")))
    }

    fn identifier(&mut self) -> Result<String, SyntaxError> {
        let mut name = String::new();
        while let Some(c) = self.peek(0) {
            if c == '\\' {
                self.bump();
                if self.bump() != Some('u') {
                    return Err(self.error("Invalid Unicode escape sequence"));
                }
                let c = self.unicode_escape()?;
                name.push(c);
            } else if is_id_part(c) {
                name.push(c);
                self.bump();
            } else {
                break;
            }
        }
        Ok(name)
    }

    fn digits(&mut self, radix: u32, out: &mut String) {
        while let Some(c) = self.peek(0) {
            if c == '_' && self.peek(1).is_some_and(|d| d.is_digit(radix)) {
                self.bump();
            } else if c.is_digit(radix) {
                out.push(c);
                self.bump();
            } else {
                break;
            }
        }
    }

    fn number(&mut self) -> Result<f64, SyntaxError> {
        let radix = match (self.peek(0), self.peek(1)) {
            (Some('0'), Some('x' | 'X')) => 16,
            (Some('0'), Some('o' | 'O')) => 8,
            (Some('0'), Some('b' | 'B')) => 2,
            _ => 10,
        };
        let value = if radix == 10 {
            let mut text = String::new();
            self.digits(10, &mut text);
            if self.peek(0) == Some('.') {
                text.push('.');
                self.bump();
                self.digits(10, &mut text);
            }
            if matches!(self.peek(0), Some('e' | 'E')) {
                let sign = self.peek(1);
                let digit_at = usize::from(matches!(sign, Some('+' | '-'))) + 1;
                if self.peek(digit_at).is_some_and(|c| c.is_ascii_digit()) {
                    text.push('e');
                    self.bump();
                    if let Some(sign @ ('+' | '-')) = self.peek(0) {
                        text.push(sign);
                        self.bump();
                    }
                    self.digits(10, &mut text);
                }
            }
            text.parse::<f64>()
                .map_err(|_| self.error("Invalid number"))?
        } else {
            self.bump();
            self.bump();
            let mut text = String::new();
            self.digits(radix, &mut text);
            if text.is_empty() {
                return Err(self.error("Invalid or unexpected token"));
            }
            text.chars().fold(0.0, |value, c| {
                value * f64::from(radix) + f64::from(c.to_digit(radix).unwrap_or(0))
            })
        };
        if self.peek(0) == Some('n') {
            return Err(self.error("BigInt literals are not supported in codemode scripts"));
        }
        if self.peek(0).is_some_and(is_id_start) {
            return Err(self.error("Invalid or unexpected token"));
        }
        Ok(value)
    }

    fn hex(&mut self, count: usize) -> Result<u32, SyntaxError> {
        let mut value = 0_u32;
        for _ in 0..count {
            let digit = self
                .bump()
                .and_then(|c| c.to_digit(16))
                .ok_or_else(|| self.error("Invalid hexadecimal escape sequence"))?;
            value = value * 16 + digit;
        }
        Ok(value)
    }

    /// After `\u`: `XXXX` (a surrogate pair as two escapes) or `{X...}`.
    fn unicode_escape(&mut self) -> Result<char, SyntaxError> {
        let code = if self.peek(0) == Some('{') {
            self.bump();
            let mut value = 0_u32;
            loop {
                match self.bump() {
                    Some('}') => break,
                    Some(c) => {
                        let digit = c
                            .to_digit(16)
                            .ok_or_else(|| self.error("Invalid Unicode escape sequence"))?;
                        value = value.saturating_mul(16).saturating_add(digit);
                    }
                    None => return Err(self.error("Invalid Unicode escape sequence")),
                }
            }
            value
        } else {
            let high = self.hex(4)?;
            if (0xD800..0xDC00).contains(&high)
                && self.peek(0) == Some('\\')
                && self.peek(1) == Some('u')
            {
                let saved = (self.pos, self.line, self.col);
                self.bump();
                self.bump();
                let low = self.hex(4)?;
                if (0xDC00..0xE000).contains(&low) {
                    0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00)
                } else {
                    (self.pos, self.line, self.col) = saved;
                    high
                }
            } else {
                high
            }
        };
        Ok(char::from_u32(code).unwrap_or('\u{fffd}'))
    }

    /// One escape after `\` in a string or template, pushed to `out`.
    fn escape(&mut self, out: &mut String) -> Result<(), SyntaxError> {
        let Some(c) = self.bump() else {
            return Err(self.error("Invalid or unexpected token"));
        };
        match c {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'v' => out.push('\u{b}'),
            '0' if !self.peek(0).is_some_and(|d| d.is_ascii_digit()) => out.push('\0'),
            'x' => {
                let code = self.hex(2)?;
                out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
            }
            'u' => out.push(self.unicode_escape()?),
            '\r' => {
                if self.peek(0) == Some('\n') {
                    self.bump();
                }
            }
            c if is_line_terminator(c) => {}
            c => out.push(c),
        }
        Ok(())
    }

    fn string(&mut self, quote: char) -> Result<String, SyntaxError> {
        self.bump();
        let mut text = String::new();
        loop {
            match self.peek(0) {
                None => return Err(self.error("Invalid or unexpected token: unterminated string")),
                Some(c) if c == quote => {
                    self.bump();
                    return Ok(text);
                }
                Some('\\') => {
                    self.bump();
                    self.escape(&mut text)?;
                }
                Some(c) if c == '\n' || c == '\r' => {
                    return Err(self.error("Invalid or unexpected token: unterminated string"));
                }
                Some(c) => {
                    text.push(c);
                    self.bump();
                }
            }
        }
    }

    fn template(&mut self) -> Result<Tok, SyntaxError> {
        self.bump();
        let mut quasis = Vec::new();
        let mut substitutions = Vec::new();
        let mut text = String::new();
        loop {
            match self.peek(0) {
                None => return Err(self.error("Unterminated template literal")),
                Some('`') => {
                    self.bump();
                    quasis.push(Rc::from(text.as_str()));
                    return Ok(Tok::Template(quasis, substitutions));
                }
                Some('\\') => {
                    self.bump();
                    self.escape(&mut text)?;
                }
                Some('$') if self.peek(1) == Some('{') => {
                    self.bump();
                    self.bump();
                    quasis.push(Rc::from(text.as_str()));
                    text.clear();
                    let mut inner = self.run(true)?;
                    let (line, col) = (self.line, self.col);
                    inner.push(Token {
                        tok: Tok::Eof,
                        line,
                        col,
                        newline_before: false,
                    });
                    substitutions.push(inner);
                }
                Some('\r') => {
                    self.bump();
                    if self.peek(0) == Some('\n') {
                        self.bump();
                    }
                    text.push('\n');
                }
                Some(c) => {
                    text.push(c);
                    self.bump();
                }
            }
        }
    }

    fn regex(&mut self) -> Result<Tok, SyntaxError> {
        self.bump();
        let mut pattern = String::new();
        let mut in_class = false;
        loop {
            match self.bump() {
                None => return Err(self.error("Invalid regular expression: missing /")),
                Some(c) if is_line_terminator(c) => {
                    return Err(self.error("Invalid regular expression: missing /"));
                }
                Some('\\') => {
                    pattern.push('\\');
                    match self.bump() {
                        Some(c) if !is_line_terminator(c) => pattern.push(c),
                        _ => return Err(self.error("Invalid regular expression: missing /")),
                    }
                }
                Some('[') => {
                    in_class = true;
                    pattern.push('[');
                }
                Some(']') => {
                    in_class = false;
                    pattern.push(']');
                }
                Some('/') if !in_class => break,
                Some(c) => pattern.push(c),
            }
        }
        let mut flags = String::new();
        while let Some(c) = self.peek(0) {
            if !is_id_part(c) {
                break;
            }
            flags.push(c);
            self.bump();
        }
        Ok(Tok::Regex(pattern.into(), flags.into()))
    }
}

/// Whether a `/` after `previous` starts a regular expression: where a value
/// cannot end.
fn regex_allowed(previous: Option<&Token>) -> bool {
    match previous.map(|token| &token.tok) {
        None => true,
        Some(Tok::Num(_) | Tok::Str(_) | Tok::Template(..) | Tok::Regex(..) | Tok::Eof) => false,
        Some(Tok::Ident(word)) => REGEX_AFTER_WORD.contains(&&**word),
        Some(Tok::Punct(punct)) => !matches!(*punct, ")" | "]" | "}" | "++" | "--"),
    }
}

#[cfg(test)]
mod tests {
    use super::{Tok, tokenize};

    fn kinds(source: &str) -> Vec<Tok> {
        tokenize(source)
            .unwrap()
            .into_iter()
            .map(|token| token.tok)
            .collect()
    }

    #[test]
    fn reads_numbers_strings_and_punctuators() {
        assert_eq!(
            kinds("a >>>= 0x1F + .5e1 - 1_000 'x\\n'"),
            vec![
                Tok::Ident("a".into()),
                Tok::Punct(">>>="),
                Tok::Num(31.0),
                Tok::Punct("+"),
                Tok::Num(5.0),
                Tok::Punct("-"),
                Tok::Num(1000.0),
                Tok::Str("x\n".into()),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn tells_regular_expressions_from_division() {
        assert!(
            matches!(kinds("x = /a\\/[/]b/gi")[2], Tok::Regex(ref p, ref f) if &**p == "a\\/[/]b" && &**f == "gi")
        );
        assert_eq!(kinds("a / b / c")[1], Tok::Punct("/"));
        assert_eq!(kinds("f(x) / 2")[4], Tok::Punct("/"));
    }

    #[test]
    fn lexes_template_substitutions_as_tokens() {
        let tokens = kinds("`a${ {b: 1}.b }c${`d${e}`}`");
        let Tok::Template(quasis, substitutions) = &tokens[0] else {
            panic!("{tokens:?}");
        };
        assert_eq!(quasis.len(), 3);
        assert_eq!(substitutions.len(), 2);
        assert_eq!(substitutions[0][0].tok, Tok::Punct("{"));
        assert!(matches!(substitutions[1][0].tok, Tok::Template(..)));
    }

    #[test]
    fn marks_line_terminators_and_lines() {
        let tokens = tokenize("a\n/* x\n */ b // c\nc").unwrap();
        assert!(!tokens[0].newline_before);
        assert!(tokens[1].newline_before);
        assert_eq!(tokens[1].line, 3);
        assert_eq!(tokens[2].line, 4);
    }

    #[test]
    fn rejects_what_codemode_does_not_read() {
        assert!(tokenize("10n").is_err());
        assert!(tokenize("'open").is_err());
        assert!(tokenize("#x").is_err());
        assert!(tokenize("`a${b").is_err());
    }
}
