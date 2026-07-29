use std::fmt::Debug;

use crate::{
    analyze::{
        Span,
        semantics::{SemanticType, Sign},
    },
    ir::ValSize,
};

pub mod parse;

#[derive(Default, Debug)]
pub struct AST {
    pub package: Option<String>,
    pub modules: Vec<(String, Span)>,
    pub items: Vec<Item>,
}

impl AST {
    pub fn new() -> Self {
        Self {
            package: None,
            modules: Vec::new(),
            items: Vec::new(),
        }
    }

    pub fn add_item(&mut self, item: Item) {
        self.items.push(item);
    }

    pub fn imports(&self) -> impl Iterator<Item = &str> {
        self.items.iter().filter_map(|i| {
            if let Item::ExternLib(lib) = i {
                Some(lib.as_str())
            } else {
                None
            }
        })
    }

    pub fn mangle(&mut self, lib: &str) {
        for item in self.items.iter_mut() {
            match item {
                Item::Function(FnDef { name, .. })
                | Item::ForwardDecl { name, .. }
                | Item::Struct { name, .. }
                | Item::Impl {
                    struct_name: name, ..
                } => *name = format!("{}::{}", lib, name),
                Item::ExternLib(_) => (),
                Item::MemorySegment { .. } => (),
            }
        }
    }
}

#[derive(Debug)]
pub enum Item {
    Function(FnDef),
    ForwardDecl {
        name: String,
        args: Vec<(String, SemanticType, Span)>,
        ret_type: SemanticType,
        decl_span: Span,
    },
    ExternLib(String),
    MemorySegment {
        name: String,
        typ: SemanticType,
    },
    Struct {
        name: String,
        decl_span: Span,
        fields: Vec<(String, SemanticType, Span)>,
    },
    Impl {
        struct_name: String,
        functions: Vec<FnDef>,
    },
}

#[derive(Debug)]
pub struct FnDef {
    pub name: String,
    pub args: Vec<(String, SemanticType, Span)>,
    pub body: Vec<Statement>,
    pub decl_span: Span,
    pub ret_type: SemanticType,
    pub ret_type_span: Span,
}

#[derive(Debug)]
pub enum Statement {
    Declare {
        var: String,
        expr: Expression,
        var_span: Span,
    },
    Assign {
        var: Assignable,
        expr: Expression,
        var_span: Span,
    },
    If {
        guard: Expression,
        body: Vec<Statement>,
    },
    Return(Expression),
    Expr(Expression),
    WhileLoop {
        guard: Expression,
        body: Vec<Statement>,
    },
}

#[derive(Debug, Clone)]
pub enum Assignable {
    Var(String),
    Ptr(String, Option<ValSize>),
    Index(String, Box<Expression>, Option<ValSize>),
    MemberAccess(Box<Expression>, String),
}

impl Assignable {
    pub fn symbol(&self) -> &str {
        match self {
            Self::Var(var)
            | Self::Ptr(var, _)
            | Self::Index(var, _, _)
            | Self::MemberAccess(_, var) => var,
        }
    }
}

#[derive(Clone)]
pub struct Expression {
    pub inner: ExprInner,
    pub typ: Option<SemanticType>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ExprInner {
    Const(u64, Option<SemanticType>),
    Character(char),
    String(String),
    Bool(bool),

    Variable(String),
    Pointer(String),
    Deref(String, Option<SemanticType>),

    Arithmetic(Box<Expression>, Box<Expression>, ArithmeticOp, Option<Sign>),
    Comparison(Box<Expression>, Box<Expression>, CompareOp, Option<Sign>),
    Logical(Box<Expression>, Box<Expression>, LogicalOp),
    Not(Box<Expression>),
    Negate(Box<Expression>),

    Cast(Box<Expression>, SemanticType),
    Index(String, Box<Expression>, Option<ValSize>),

    MemberAccess(Box<Expression>, String, Option<String>),

    FnCall(String, Vec<Expression>),

    SizeOf(SemanticType),
}

#[derive(Debug, Clone, Copy)]
pub enum ArithmeticOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

#[derive(Debug, Clone, Copy)]
pub enum CompareOp {
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
}

#[derive(Debug, Clone, Copy)]
pub enum LogicalOp {
    And,
    Or,
}

impl Debug for Expression {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self.inner, f)
    }
}
