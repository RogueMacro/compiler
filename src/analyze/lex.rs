use std::{ops::Range, path::PathBuf, rc::Rc, str::Chars};

use crate::analyze::{
    Error, ErrorContext,
    lex::token::{Keyword, Operator, Token},
    semantics::types::Primitive,
};

pub mod token;

pub struct Tokens<'s> {
    tokens: std::vec::IntoIter<(Token<'s>, Range<usize>)>,
    last: Option<(Token<'s>, Range<usize>)>,
    current: Option<(Token<'s>, Range<usize>)>,
    next: Option<(Token<'s>, Range<usize>)>,
}

impl<'s> Tokens<'s> {
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
    pub fn current(&self) -> Option<&(Token<'s>, Range<usize>)> {
        self.current.as_ref()
    }

    pub fn take_current(&mut self) -> Option<(Token<'s>, Range<usize>)> {
        self.move_one();
        self.last.clone()
    }

    /// Lookahead to next token
    pub fn peek(&self) -> Option<&(Token<'s>, Range<usize>)> {
        self.next.as_ref()
    }

    /// Move on from current token to the next
    pub fn move_one(&mut self) {
        self.last = self.current.take();
        self.current = self.next.take();
        self.next = self.tokens.next();
    }
}

pub struct Lexer<'e, 's> {
    chars: CharIter<'s>,
    source: &'s str,
    err_ctx: &'e mut ErrorContext,
    src_path: Rc<PathBuf>,
}

impl<'e, 's> Lexer<'e, 's> {
    pub fn lex(
        source: &'s str,
        src_path: Rc<PathBuf>,
        err_ctx: &'e mut ErrorContext,
    ) -> Result<Tokens<'s>, ()> {
        let mut lexer = Self {
            chars: CharIter::new(source.chars()),
            source,
            err_ctx,
            src_path,
        };

        let mut tokens = Vec::new();
        while let Some(token) = lexer.lex_next().map_err(|_| ())? {
            tokens.push(token);
        }

        let mut tokens = Tokens::<'s> {
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

/// Internals
impl<'e, 's> Lexer<'e, 's> {
    fn find_next_lexable(&mut self) {
        while let Some(c) = self.chars.cur {
            if c == '/' && self.chars.peek == Some('/') {
                self.lex_comment();
            } else if c.is_whitespace() {
                self.chars.next();
            } else {
                break;
            }
        }
    }

    fn lex_next(&mut self) -> Result<Option<(Token<'s>, Range<usize>)>, Error> {
        self.find_next_lexable();

        let Some(c) = self.chars.cur else {
            return Ok(None);
        };

        let token_atom = Token::parse_atom(c, self.chars.peek);
        let op = Operator::parse(c, self.chars.peek);

        match (token_atom, op) {
            (Some((token, true)), _) => {
                self.chars.next2();
                return Ok(Some((token, (self.chars.index - 2)..self.chars.index)));
            }
            (_, Some((op, true))) => {
                self.chars.next2();
                return Ok(Some((
                    Token::Operator(op),
                    (self.chars.index - 2)..self.chars.index,
                )));
            }
            (Some((token, false)), _) => {
                self.chars.next();
                return Ok(Some((token, (self.chars.index - 1)..self.chars.index)));
            }
            (_, Some((op, false))) => {
                self.chars.next();
                return Ok(Some((
                    Token::Operator(op),
                    (self.chars.index - 1)..self.chars.index,
                )));
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
            self.chars.next();
            let Some(character) = self.chars.cur else {
                return Err(self
                    .err_ctx
                    .unexpected_eof(self.span((self.chars.index - 1)..self.chars.index))
                    .finish());
            };

            let character = self.lex_full_char(character)?;

            self.chars.next();
            if !matches!(self.chars.cur, Some('\'')) {
                return Err(self
                    .err_ctx
                    .unexpected_token(
                        self.span(self.chars.index..(self.chars.index + 1)),
                        format!("expected ' (quote), got '{:?}'", self.chars.cur),
                    )
                    .finish());
            }

            self.chars.next();
            return Ok(Some((
                Token::Character(character),
                (self.chars.index - 3)..self.chars.index,
            )));
        }

        if c == '"' {
            return self.lex_string().map(Some);
        }

        Err(self
            .err_ctx
            .unexpected_token(
                self.span(self.chars.index..(self.chars.index + 1)),
                "unexpected character",
            )
            .finish())
    }

    fn lex_string(&mut self) -> Result<(Token<'s>, Range<usize>), Error> {
        assert!(self.chars.cur == Some('\"'));

        let start = self.chars.index;
        self.chars.next();
        let mut string = String::new();
        while let Some(c) = self.chars.cur {
            if c == '"' {
                self.chars.next();
                return Ok((Token::String(string), start..self.chars.index));
            }

            let c = self.lex_full_char(c)?;
            string.push(c);

            self.chars.next();
        }

        Err(self
            .err_ctx
            .unexpected_eof(self.span(start..self.chars.index))
            .finish())
    }

    fn lex_full_char(&mut self, c: char) -> Result<char, Error> {
        if c == '\\' {
            self.chars.next();
            let Some(next) = self.chars.cur else {
                return Err(self
                    .err_ctx
                    .unexpected_eof(self.span((self.chars.index - 1)..self.chars.index))
                    .finish());
            };

            let escaped = match next {
                '\\' => '\\',
                '"' => '"',
                '\'' => '\'',
                'n' => '\n',
                '0' => '\0',
                _ => {
                    let span = self.span((self.chars.index - 1)..self.chars.index);
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
            let span = self.span((self.chars.index - 1)..self.chars.index);
            Err(self
                .err_ctx
                .error(span.clone())
                .with_message("invalid string")
                .with_label(span, "not a valid character")
                .finish())
        }
    }

    fn lex_ascii(&mut self) -> (Token<'s>, Range<usize>) {
        let start = self.chars.index;
        while let Some(c) = self.chars.cur
            && (c.is_ascii_alphanumeric() || c == '_')
        {
            self.chars.next();
        }

        let string = &self.source[start..self.chars.index];

        let token = if let Some(keyword) = Keyword::parse(string) {
            Token::Keyword(keyword)
        } else if let Ok(b) = string.parse::<bool>() {
            Token::Bool(b)
        } else {
            Token::Ident(string)
        };

        (token, start..self.chars.index)
    }

    fn lex_number(&mut self) -> Result<(Token<'s>, Range<usize>), Error> {
        let start = self.chars.index;
        let mut string = String::new();
        while let Some(c) = self.chars.cur
            && c.is_ascii_digit()
        {
            string.push(c);
            self.chars.next();
        }

        let num: u64 = string.parse().unwrap();
        let mut explicit_type = None;

        if let Some(sign_char @ ('i' | 'u')) = self.chars.cur {
            let begin = self.chars.index;
            let size_chars = self.chars.next2().ok_or_else(|| {
                let span = self.span(begin..begin + 1);
                self.err_ctx.unexpected_eof(span).finish()
            })?;
            if !matches!(size_chars, ('6', '4')) {
                let span = self.span(begin..self.chars.index);
                return Err(self
                    .err_ctx
                    .error(span.clone())
                    .with_message("invalid number type specifier")
                    .with_label(span, "expected u64 or i64")
                    .finish());
            }

            self.chars.next();
            let type_specifier = match (sign_char, size_chars) {
                ('i', ('6', '4')) => Primitive::I64,
                ('u', ('6', '4')) => Primitive::U64,
                _ => unreachable!(),
            };

            explicit_type = Some(type_specifier);
        }

        Ok((Token::Number(num, explicit_type), start..self.chars.index))
    }

    fn lex_comment(&mut self) {
        while let Some(c) = self.chars.peek {
            self.chars.next();
            if c == '\n' {
                break;
            }
        }
    }

    fn span(&self, range: Range<usize>) -> (Rc<PathBuf>, Range<usize>) {
        (self.src_path.clone(), range)
    }
}

struct CharIter<'s> {
    cur: Option<char>,
    peek: Option<char>,
    iter: Chars<'s>,
    index: usize,
}

impl<'s> CharIter<'s> {
    pub fn new(mut chars: Chars<'s>) -> Self {
        Self {
            cur: chars.next(),
            peek: chars.next(),
            iter: chars,
            index: 0,
        }
    }

    pub fn next(&mut self) -> Option<char> {
        self.cur = self.peek;
        self.peek = self.iter.next();
        self.index += 1;
        self.cur
    }

    pub fn next2(&mut self) -> Option<(char, char)> {
        let a = self.next();
        let b = self.next();

        if a.is_some() && b.is_some() {
            Some((a.unwrap(), b.unwrap()))
        } else {
            None
        }
    }

    pub fn next3(&mut self) -> Option<(char, char, char)> {
        let a = self.next();
        let b = self.next();
        let c = self.next();

        if a.is_some() && b.is_some() && c.is_some() {
            Some((a.unwrap(), b.unwrap(), c.unwrap()))
        } else {
            None
        }
    }
}
