use std::{
    fmt::{self, Display},
    fs, io,
    ops::{Deref, Range},
    path::PathBuf,
    rc::Rc,
};

use ariadne::{Cache, ColorGenerator, Label, Report, ReportBuilder, ReportKind, Source};

pub mod ast;
pub mod lex;
pub mod semantics;

pub type Span = (Rc<PathBuf>, Range<usize>);

#[derive(Debug)]
pub struct Error(Box<Report<'static, Span>>);

impl Error {
    pub fn new(report: Report<'static, Span>) -> Self {
        Self(Box::new(report))
    }
}

impl Deref for Error {
    type Target = Report<'static, Span>;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

pub struct ErrorBuilder<'c> {
    builder: ReportBuilder<'static, Span>,
    context: &'c mut ErrorContext,
}

impl<'c> ErrorBuilder<'c> {
    pub fn with_code(mut self, code: ErrorCode) -> Self {
        self.builder = self.builder.with_code(code);
        self
    }

    pub fn with_message(mut self, msg: impl ToString) -> Self {
        self.builder.set_message(msg);
        self
    }

    pub fn with_label(mut self, span: Span, msg: impl ToString) -> Self {
        let label = Label::new(span)
            .with_color(ariadne::Color::BrightRed)
            // .with_color(self.context.color_gen.next())
            .with_message(msg);

        self.builder.add_label(label);

        self
    }

    pub fn report(self) {
        let error = Error::new(self.builder.finish());
        self.context.errors.push(error);
    }

    pub fn finish(self) -> Error {
        Error::new(self.builder.finish())
    }
}

#[derive(Default)]
pub struct ErrorContext {
    color_gen: ColorGenerator,
    errors: Vec<Error>,
}

impl ErrorContext {
    pub fn new() -> Self {
        Self {
            color_gen: ColorGenerator::new(),
            errors: Vec::new(),
        }
    }

    pub fn unexpected_token(&mut self, span: Span, message: impl ToString) -> ErrorBuilder<'_> {
        self.error(span.clone())
            .with_code(ErrorCode::UnexpectedToken)
            .with_message("unexpected token")
            .with_label(span, message)
    }

    pub fn unexpected_eof(&mut self, span: Span) -> ErrorBuilder<'_> {
        self.error(span.clone())
            .with_code(ErrorCode::UnexpectedToken)
            .with_message("unexpected end of file")
            .with_label(span, "why stop here??")
    }

    pub fn error(&mut self, span: Span) -> ErrorBuilder<'_> {
        let builder = Report::build(ReportKind::Error, span);

        ErrorBuilder {
            builder,
            context: self,
        }
    }

    pub fn warn(&mut self, span: Span) -> ErrorBuilder<'_> {
        let builder = Report::build(ReportKind::Warning, span);

        ErrorBuilder {
            builder,
            context: self,
        }
    }

    pub fn report(&mut self, error: Error) {
        self.errors.push(error);
    }

    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    pub fn errors(&self) -> &[Error] {
        &self.errors
    }

    pub fn take_errors(&mut self) -> ErrorVec {
        ErrorVec(std::mem::take(&mut self.errors))
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug)]
pub enum ErrorCode {
    MissingSemicolon,
    UnexpectedToken,
}

impl Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "E{:02}", *self as u32)
    }
}

pub struct ErrorVec(pub Vec<Error>);

impl ErrorVec {
    /// Prints all errors to stderr
    pub fn dump(&self) {
        for error in &self.0 {
            error
                .eprint(&mut Files::default())
                .expect("couldn't print error message to stderr");

            eprintln!();
        }
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl From<Error> for ErrorVec {
    fn from(error: Error) -> Self {
        Self(vec![error])
    }
}

impl fmt::Debug for ErrorVec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "failed to compile due to {} errors", self.0.len())
    }
}

#[derive(Default)]
pub struct Files {
    buffer: Option<Source>,
}

impl Cache<Rc<PathBuf>> for &mut Files {
    type Storage = String;

    fn fetch(
        &mut self,
        path: &Rc<PathBuf>,
    ) -> Result<&ariadne::Source<Self::Storage>, impl fmt::Debug> {
        self.buffer = Some(Source::from(fs::read_to_string(path.as_ref())?));
        Ok::<_, io::Error>(self.buffer.as_ref().unwrap())
    }

    fn display<'a>(&self, path: &'a Rc<PathBuf>) -> Option<impl fmt::Display + 'a> {
        // id.file_stem().and_then(OsStr::to_str)
        Some(path.display())
    }
}
