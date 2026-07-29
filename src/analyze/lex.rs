use std::{ops::Range, path::PathBuf, rc::Rc};

use crate::analyze::{
    Error, ErrorContext,
    lex::token::{Keyword, Operator, Token},
    semantics::SemanticType,
};

pub mod token;

pub struct Lexer<'e> {
    code: Vec<char>,
    index: usize,
    err_ctx: &'e mut ErrorContext,
    src_path: Rc<PathBuf>,
}

impl<'e> Lexer<'e> {
    pub fn lex(
        code: impl AsRef<str>,
        src_path: Rc<PathBuf>,
        err_ctx: &'e mut ErrorContext,
    ) -> Result<Tokens, ()> {
        let code: Vec<char> = code.as_ref().chars().collect();

        let mut lexer = Self {
            code,
            index: 0,
            err_ctx,
            src_path,
        };

        let mut tokens = Vec::new();
        while let Some(token) = lexer.lex_next().map_err(|_| ())? {
            tokens.push(token);
        }

        let mut tokens = Tokens {
            tokens: tokens.into_iter(),
            last: None,
            current: None,
            next: None,
        };

        tokens.move_one();
        tokens.move_one();

        Ok(tokens)
    }
}

pub struct Tokens {
    tokens: std::vec::IntoIter<(Token, Range<usize>)>,
    last: Option<(Token, Range<usize>)>,
    current: Option<(Token, Range<usize>)>,
    next: Option<(Token, Range<usize>)>,
}

impl Tokens {
    pub fn cur_token_start(&self) -> usize {
        self.current
            .as_ref()
            .map(|(_, r)| r.start)
            .expect("no current token")
    }

    pub fn last_token_end(&self) -> usize {
        self.last
            .as_ref()
            .map(|(_, r)| r.end)
            .expect("no last token")
    }

    /// Get current token
    pub fn current(&self) -> Option<&(Token, Range<usize>)> {
        self.current.as_ref()
    }

    pub fn take_current(&mut self) -> Option<(Token, Range<usize>)> {
        self.move_one();
        self.last.clone()
    }

    /// Lookahead to next token
    pub fn peek(&self) -> Option<&(Token, Range<usize>)> {
        self.next.as_ref()
    }

    /// Move on from current token to the next
    pub fn move_one(&mut self) {
        self.last = self.current.take();
        self.current = self.next.take();
        self.next = self.tokens.next();
    }
}

/// Internals
impl<'e> Lexer<'e> {
    fn peek_char(&self) -> Option<char> {
        self.code.get(self.index + 1).copied()
    }

    fn cur_char(&self) -> Option<char> {
        self.code.get(self.index).copied()
    }

    fn find_next_lexable(&mut self) {
        while let Some(c) = self.cur_char() {
            if c == '/' && self.peek_char() == Some('/') {
                self.lex_comment();
            } else if c.is_whitespace() {
                self.index += 1;
            } else {
                break;
            }
        }
    }

    fn lex_next(&mut self) -> Result<Option<(Token, Range<usize>)>, Error> {
        self.find_next_lexable();

        let Some(c) = self.cur_char() else {
            return Ok(None);
        };

        let token_atom = Token::parse_atom(c, self.peek_char());
        let op = Operator::parse(c, self.peek_char());

        match (token_atom, op) {
            (Some((token, true)), _) => {
                self.index += 2;
                return Ok(Some((token, (self.index - 2)..self.index)));
            }
            (_, Some((op, true))) => {
                self.index += 2;
                return Ok(Some((Token::Operator(op), (self.index - 2)..self.index)));
            }
            (Some((token, false)), _) => {
                self.index += 1;
                return Ok(Some((token, (self.index - 1)..self.index)));
            }
            (_, Some((op, false))) => {
                self.index += 1;
                return Ok(Some((Token::Operator(op), (self.index - 1)..self.index)));
            }
            _ => (),
        }

        if c.is_ascii_alphabetic() || c == '_' {
            return Ok(Some(self.lex_ascii()));
        }

        if c.is_ascii_digit() {
            return self.lex_number().map(Some);
        }

        if c == '\'' {
            self.index += 1;
            let Some(character) = self.cur_char() else {
                return Err(self
                    .err_ctx
                    .unexpected_eof(self.span((self.index - 1)..self.index))
                    .finish());
            };

            let character = self.lex_full_char(character)?;

            self.index += 1;
            if !matches!(self.cur_char(), Some('\'')) {
                return Err(self
                    .err_ctx
                    .unexpected_token(
                        self.span(self.index..(self.index + 1)),
                        format!("expected ' (quote), got '{:?}'", self.cur_char()),
                    )
                    .finish());
            }

            self.index += 1;
            return Ok(Some((
                Token::Character(character),
                (self.index - 3)..self.index,
            )));
        }

        if c == '"' {
            return self.lex_string().map(Some);
        }

        Err(self
            .err_ctx
            .unexpected_token(
                self.span(self.index..(self.index + 1)),
                "unexpected character",
            )
            .finish())
    }

    fn lex_string(&mut self) -> Result<(Token, Range<usize>), Error> {
        assert!(self.cur_char() == Some('\"'));

        let start = self.index;
        self.index += 1;
        let mut string = String::new();
        while let Some(c) = self.cur_char() {
            if c == '"' {
                self.index += 1;
                return Ok((Token::String(string), start..self.index));
            }

            let c = self.lex_full_char(c)?;
            string.push(c);

            self.index += 1;
        }

        Err(self
            .err_ctx
            .unexpected_eof(self.span(start..self.index))
            .finish())
    }

    fn lex_full_char(&mut self, c: char) -> Result<char, Error> {
        if c == '\\' {
            self.index += 1;
            let Some(next) = self.cur_char() else {
                return Err(self
                    .err_ctx
                    .unexpected_eof(self.span((self.index - 1)..self.index))
                    .finish());
            };

            let escaped = match next {
                '\\' => '\\',
                '"' => '"',
                '\'' => '\'',
                'n' => '\n',
                '0' => '\0',
                _ => {
                    let span = self.span((self.index - 1)..self.index);
                    return Err(self
                        .err_ctx
                        .error(span.clone())
                        .with_message("invalid escape character")
                        .with_label(span, "this is not a valid escape character")
                        .finish());
                }
            };

            Ok(escaped)
        } else if c.is_ascii() {
            Ok(c)
        } else {
            let span = self.span((self.index - 1)..self.index);
            Err(self
                .err_ctx
                .error(span.clone())
                .with_message("invalid string")
                .with_label(span, "not a valid character")
                .finish())
        }
    }

    fn lex_ascii(&mut self) -> (Token, Range<usize>) {
        let start = self.index;
        let mut string = String::new();
        while let Some(c) = self.cur_char()
            && (c.is_ascii_alphanumeric() || c == '_')
        {
            string.push(c);
            self.index += 1;
        }

        let token = if let Some(keyword) = Keyword::parse(&string) {
            Token::Keyword(keyword)
        } else if let Ok(b) = string.parse::<bool>() {
            Token::Bool(b)
        } else {
            Token::Ident(string)
        };

        (token, start..self.index)
    }

    fn lex_number(&mut self) -> Result<(Token, Range<usize>), Error> {
        let start = self.index;
        let mut string = String::new();
        while let Some(c) = self.cur_char()
            && c.is_ascii_digit()
        {
            string.push(c);
            self.index += 1;
        }

        let num: u64 = string.parse().unwrap();
        let mut explicit_type = None;

        if matches!(self.cur_char(), Some('i' | 'u')) {
            let begin = self.index;
            self.index += 3;
            let type_specifier = &self.code[begin..self.index];
            if !matches!(type_specifier, ['u' | 'i', '6', '4']) {
                let span = self.span(begin..self.index);
                return Err(self
                    .err_ctx
                    .error(span.clone())
                    .with_message("invalid number type specifier")
                    .with_label(span, "expected u64 or i64")
                    .finish());
            }

            explicit_type = Some(SemanticType::from(&String::from_iter(type_specifier)));
        }

        Ok((Token::Number(num, explicit_type), start..self.index))
    }

    fn lex_comment(&mut self) {
        while let Some(c) = self.peek_char() {
            self.index += 1;
            if c == '\n' {
                break;
            }
        }
    }

    fn span(&self, range: Range<usize>) -> (Rc<PathBuf>, Range<usize>) {
        (self.src_path.clone(), range)
    }
}
