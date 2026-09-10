use std::{borrow::Cow, fmt::Debug};

use crate::{
    analyze::{
        Span,
        semantics::types::{ParsedType, Primitive, Sign, TypeId},
    },
    ir::ValSize,
};

pub mod parse;

#[derive(Default, Debug)]
pub struct AST<'s, T> {
    pub package: Option<&'s str>,
    pub modules: Vec<(&'s str, Span)>,
    pub imports: Vec<(&'s str, Span)>,
    pub mangled_path: Option<String>,
    pub items: Vec<Item<'s, T>>,
}

impl<'s, T> AST<'s, T> {
    pub fn new() -> Self {
        Self {
            package: None,
            modules: Vec::new(),
            imports: Vec::new(),
            mangled_path: None,
            items: Vec::new(),
        }
    }

    pub fn add_item(&mut self, item: Item<'s, T>) {
        self.items.push(item);
    }

    pub fn mangle(&mut self, lib: impl Into<String>) {
        let lib = lib.into();

        for item in self.items.iter_mut() {
            match item {
                Item::Function(FnDef { name, .. })
                | Item::ForwardDecl { name, .. }
                | Item::Struct { name, .. }
                | Item::Impl {
                    struct_name: name, ..
                } => *name = Cow::Owned(format!("{}::{}", lib, name)),
                Item::ExternLib(_) => (),
                Item::MemorySegment { .. } => (),
            }
        }

        self.mangled_path = Some(lib);
    }
}

#[derive(Debug)]
pub enum Item<'s, T> {
    Function(FnDef<'s, T>),
    ForwardDecl {
        name: Cow<'s, str>,
        args: Vec<(&'s str, T, Span)>,
        ret_type: T,
        decl_span: Span,
    },
    ExternLib(&'s str),
    MemorySegment {
        name: &'s str,
        typ: T,
    },
    Struct {
        name: Cow<'s, str>,
        decl_span: Span,
        fields: Vec<(&'s str, T, Span)>,
    },
    Impl {
        struct_name: Cow<'s, str>,
        functions: Vec<FnDef<'s, T>>,
    },
}

#[derive(Debug)]
pub struct FnDef<'s, T> {
    pub name: Cow<'s, str>,
    pub args: Vec<(&'s str, T, Span)>,
    pub body: Vec<Statement<'s, T>>,
    pub decl_span: Span,
    pub ret_type: T,
}

#[derive(Debug)]
pub enum Statement<'s, T> {
    Declare {
        var: &'s str,
        expr: Expression<'s, T>,
        var_span: Span,
        explicit_type: Option<T>,
    },
    Assign {
        var: Assignable<'s, T>,
        expr: Expression<'s, T>,
        var_span: Span,
    },
    If {
        guard: Expression<'s, T>,
        body: Vec<Statement<'s, T>>,
    },
    Return(Expression<'s, T>),
    Expr(Expression<'s, T>),
    WhileLoop {
        guard: Expression<'s, T>,
        body: Vec<Statement<'s, T>>,
    },
}

#[derive(Debug, Clone)]
pub enum Assignable<'s, T> {
    Var(&'s str),
    Ptr(&'s str, Option<ValSize>),
    Index {
        data: Box<Expression<'s, T>>,
        index: Box<Expression<'s, T>>,
        val_size: Option<ValSize>,
    },
    MemberAccess(Box<Expression<'s, T>>, &'s str),
}

impl<'s, T> Assignable<'s, T> {
    // pub fn symbol(&self) -> &'s str {
    //     match self {
    //         Self::Var(var)
    //         | Self::Ptr(var, _)
    //         | Self::Index{var, _, _)
    //         | Self::MemberAccess(_, var) => var,
    //     }
    // }
}

#[derive(Clone)]
pub struct Expression<'s, T> {
    pub inner: ExprInner<'s, T>,
    pub typ: Option<T>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ExprInner<'s, T> {
    Const(u64, Option<Primitive>),
    Character(char),
    String(String),
    Bool(bool),

    Ident(&'s str),
    Pointer(&'s str),
    Deref(&'s str, Option<TypeId>),

    Arithmetic(
        Box<Expression<'s, T>>,
        Box<Expression<'s, T>>,
        ArithmeticOp,
        Option<Sign>,
    ),
    Comparison(
        Box<Expression<'s, T>>,
        Box<Expression<'s, T>>,
        CompareOp,
        Option<Sign>,
    ),
    Logical(Box<Expression<'s, T>>, Box<Expression<'s, T>>, LogicalOp),
    Not(Box<Expression<'s, T>>),
    Negate(Box<Expression<'s, T>>),

    Cast(Box<Expression<'s, T>>, T),

    Index {
        data: Box<Expression<'s, T>>,
        index: Box<Expression<'s, T>>,
        val_size: Option<ValSize>,
    },

    MemberAccess(Box<Expression<'s, T>>, &'s str, Option<TypeId>),

    FnCall(FnPtr<'s, T>, Vec<Expression<'s, T>>),

    Construct {
        typ: T,
        fields: Vec<(&'s str, Expression<'s, T>)>,
    },

    SizeOf(T),
}

#[derive(Debug, Clone)]
pub enum FnPtr<'s, T> {
    Named(Cow<'s, str>),
    Expr(Box<Expression<'s, T>>),
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

impl<'s, T: Debug> Debug for Expression<'s, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self.inner, f)
    }
}
