use crate::analyze::{ast::ArithmeticOp, semantics::SemanticType};

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Keyword(Keyword),

    Number(u64, Option<SemanticType>),
    Character(char),
    String(String),
    Bool(bool),

    Ident(String),

    Semicolon,
    Colon,
    Comma,
    LeftParenthesis,
    RightParenthesis,
    LeftCurlyBracket,
    RightCurlyBracket,
    LeftBracket,
    RightBracket,

    Reference,

    Declare,
    Assign(Option<Operator>),
    Arrow,
    PathSeparator,

    Operator(Operator),
}

impl Token {
    pub fn parse_atom(current: char, lookahead: Option<char>) -> Option<(Self, bool)> {
        let token = match (current, lookahead) {
            (':', Some('=')) => (Self::Declare, true),
            (':', Some(':')) => (Self::PathSeparator, true),
            ('-', Some('>')) => (Self::Arrow, true),
            ('+', Some('=')) => (Self::Assign(Some(Operator::Plus)), true),
            ('-', Some('=')) => (Self::Assign(Some(Operator::Minus)), true),
            ('*', Some('=')) => (Self::Assign(Some(Operator::Star)), true),
            ('/', Some('=')) => (Self::Assign(Some(Operator::Slash)), true),

            ('=', _) => (Self::Assign(None), false),
            (';', _) => (Self::Semicolon, false),
            (':', _) => (Self::Colon, false),
            (',', _) => (Self::Comma, false),
            ('(', _) => (Self::LeftParenthesis, false),
            (')', _) => (Self::RightParenthesis, false),
            ('{', _) => (Self::LeftCurlyBracket, false),
            ('}', _) => (Self::RightCurlyBracket, false),
            ('[', _) => (Self::LeftBracket, false),
            (']', _) => (Self::RightBracket, false),

            ('&', _) => (Self::Reference, false),

            _ => return None,
        };

        Some(token)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Operator {
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,

    Plus,
    Minus,
    Star,
    Slash,
    Modulo,

    Not,

    And,
    Or,

    Dot,
}

impl Operator {
    pub fn parse(current: char, lookahead: Option<char>) -> Option<(Self, bool)> {
        let op = match (current, lookahead) {
            ('=', Some('=')) => (Self::Equal, true),
            ('!', Some('=')) => (Self::NotEqual, true),
            ('<', Some('=')) => (Self::LessOrEqual, true),
            ('>', Some('=')) => (Self::GreaterOrEqual, true),

            ('&', Some('&')) => (Self::And, true),
            ('|', Some('|')) => (Self::Or, true),

            ('<', _) => (Self::Less, false),
            ('>', _) => (Self::Greater, false),

            ('+', _) => (Self::Plus, false),
            ('-', _) => (Self::Minus, false),
            ('*', _) => (Self::Star, false),
            ('/', _) => (Self::Slash, false),
            ('%', _) => (Self::Modulo, false),

            ('!', _) => (Self::Not, false),

            ('.', _) => (Self::Dot, false),

            _ => return None,
        };

        Some(op)
    }

    pub fn precedence(&self) -> i32 {
        use Operator::*;

        match self {
            Or => 0,
            And => 1,
            Equal | NotEqual | Less | LessOrEqual | Greater | GreaterOrEqual => 2,
            Plus | Minus => 3,
            Star | Slash | Modulo => 4,
            Not => 5,
            Dot => 6,
        }
    }

    pub fn as_arithmetic(&self) -> Option<ArithmeticOp> {
        let op = match self {
            Operator::Plus => ArithmeticOp::Add,
            Operator::Minus => ArithmeticOp::Sub,
            Operator::Star => ArithmeticOp::Mul,
            Operator::Slash => ArithmeticOp::Div,
            _ => return None,
        };

        Some(op)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Keyword {
    Package,
    Module,
    Use,
    Extern,

    Memory,
    Function,
    Struct,
    Impl,

    If,
    While,
    For,
    In,
    Return,

    As,
    SizeOf,
}

impl Keyword {
    pub fn parse(value: impl AsRef<str>) -> Option<Self> {
        let keyword = match value.as_ref() {
            "fn" => Keyword::Function,
            "return" => Keyword::Return,
            "if" => Keyword::If,
            "use" => Keyword::Use,
            "extern" => Keyword::Extern,
            "as" => Keyword::As,
            "while" => Keyword::While,
            "for" => Keyword::For,
            "in" => Keyword::In,
            "memory" => Keyword::Memory,
            "struct" => Keyword::Struct,
            "impl" => Keyword::Impl,
            "sizeof" => Keyword::SizeOf,
            "package" => Keyword::Package,
            "mod" => Keyword::Module,
            _ => return None,
        };

        Some(keyword)
    }
}
