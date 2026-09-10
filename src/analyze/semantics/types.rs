use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    fmt,
    hash::{Hash, Hasher},
    sync::OnceLock,
};

use rustc_hash::FxHasher;

use crate::analyze::{
    ErrorContext, Span,
    ast::{AST, Assignable, ExprInner, Expression, FnDef, FnPtr, Item, Statement},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeId(u64);

impl TypeId {
    pub fn unit() -> Self {
        static ID: OnceLock<TypeId> = OnceLock::new();
        *ID.get_or_init(|| TypeId::from_parsed(&ParsedType::Primitive(Primitive::Unit)))
    }

    pub fn i64() -> Self {
        static ID: OnceLock<TypeId> = OnceLock::new();
        *ID.get_or_init(|| TypeId::from_parsed(&ParsedType::Primitive(Primitive::I64)))
    }

    pub fn u64() -> Self {
        static ID: OnceLock<TypeId> = OnceLock::new();
        *ID.get_or_init(|| TypeId::from_parsed(&ParsedType::Primitive(Primitive::U64)))
    }

    pub fn char() -> Self {
        static ID: OnceLock<TypeId> = OnceLock::new();
        *ID.get_or_init(|| TypeId::from_parsed(&ParsedType::Primitive(Primitive::Char)))
    }

    pub fn char_ptr() -> Self {
        static ID: OnceLock<TypeId> = OnceLock::new();
        *ID.get_or_init(|| {
            TypeId::from_parsed(&ParsedType::Pointer(Box::new(ParsedType::Primitive(
                Primitive::Char,
            ))))
        })
    }

    pub fn bool() -> Self {
        static ID: OnceLock<TypeId> = OnceLock::new();
        *ID.get_or_init(|| TypeId::from_parsed(&ParsedType::Primitive(Primitive::Bool)))
    }

    /// If the type is not concretely decided, like an integer constant, the type can be switched to
    /// another compatible type depending on the context.
    pub fn compatible_with(&self, other: TypeId) -> bool {
        let t_u64 = TypeId::u64();
        let t_i64 = TypeId::i64();

        (*self == t_u64 || *self == t_i64) && (other == t_u64 || other == t_i64)
    }

    pub fn from_parsed(typ: &ParsedType) -> Self {
        let mut hasher = FxHasher::default();

        match typ {
            ParsedType::Primitive(primitive) => primitive.hash(&mut hasher),
            ParsedType::Pointer(value_type) => {
                let value_typeid = TypeId::from_parsed(value_type);
                hasher.write_u64(value_typeid.0);
                hasher.write_u64(PTR_HASH_VAL);
            }
            ParsedType::Struct(qualifier) => qualifier.hash(&mut hasher),
        }

        TypeId(hasher.finish())
    }

    pub fn from_primitive(primitive: Primitive) -> Self {
        let mut hasher = FxHasher::default();
        primitive.hash(&mut hasher);
        TypeId(hasher.finish())
    }

    fn to_ptr_type(self) -> TypeId {
        let mut hasher = FxHasher::default();
        hasher.write_u64(self.0);
        hasher.write_u64(PTR_HASH_VAL);
        TypeId(hasher.finish())
    }
}

#[derive(Debug, Clone)]
pub struct TypeInfo {
    pub size: u64,
    pub kind: TypeKind,
}

impl TypeInfo {
    pub fn sign(&self) -> Option<Sign> {
        if let TypeKind::Primitive(primitive) = self.kind {
            primitive.sign()
        } else {
            None
        }
    }

    pub fn is_ptr(&self) -> bool {
        matches!(self.kind, TypeKind::Pointer(_))
    }

    pub fn deref_type(&self) -> Option<TypeId> {
        if let TypeKind::Pointer(deref_type) = self.kind {
            Some(deref_type)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone)]
pub enum TypeKind {
    Struct {
        qualifier: String,
        fields: Vec<(String, TypeId, u64, Span)>,
    },
    Pointer(TypeId),
    Primitive(Primitive),
}

pub struct TypeMap {
    map: HashMap<TypeId, TypeInfo>,
}

const PTR_HASH_VAL: u64 = 1779033703;

impl TypeMap {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    // pub fn make_typeid<F: FnOnce() -> TypeInfo>(&mut self, qualifier: &str, default: F) -> TypeId {
    //     let typeid = TypeId::from_str(qualifier);
    //     self.get_or_insert_with(typeid, default);
    //     typeid
    // }

    pub fn get(&self, typeid: TypeId) -> &TypeInfo {
        self.map.get(&typeid).expect("unknown typeid")
    }

    pub fn get_or_insert_with<F: FnOnce() -> TypeInfo>(
        &mut self,
        typeid: TypeId,
        default: F,
    ) -> &TypeInfo {
        self.map.entry(typeid).or_insert_with(default)
    }

    pub fn ptr_type_to(&mut self, typeid: TypeId) -> TypeId {
        let ptr_typeid = typeid.to_ptr_type();

        self.map.entry(ptr_typeid).or_insert_with(|| TypeInfo {
            size: 8,
            kind: TypeKind::Pointer(typeid),
        });

        ptr_typeid
    }

    pub fn size_of(&self, typeid: TypeId) -> u64 {
        self.get(typeid).size
    }

    pub fn offset_of_member(&self, typeid: TypeId, member: &str) -> Option<u64> {
        if let Some(TypeInfo {
            kind: TypeKind::Struct { fields, .. },
            ..
        }) = self.map.get(&typeid)
        {
            for (field_name, _, offset, _) in fields.iter() {
                if field_name == member {
                    return Some(*offset);
                }
            }
        }

        None
    }

    pub fn display(&self, typeid: TypeId) -> NameDisplay<'_> {
        NameDisplay {
            types: self,
            typeid,
        }
    }
}

pub struct NameDisplay<'t> {
    types: &'t TypeMap,
    typeid: TypeId,
}

impl<'t> fmt::Display for NameDisplay<'t> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut typeid = self.typeid;
        loop {
            let typeinfo = self.types.get(typeid);
            match &typeinfo.kind {
                TypeKind::Struct { qualifier, .. } => {
                    f.write_str(qualifier)?;
                }
                TypeKind::Pointer(value_typeid) => {
                    write!(f, "*")?;
                    typeid = *value_typeid;
                    continue;
                }
                TypeKind::Primitive(primitive) => {
                    write!(f, "{}", primitive)?;
                }
            }

            break;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Hash)]
pub enum Primitive {
    Unit,
    I8,
    I64,
    U8,
    U64,
    Char,
    Bool,
}

impl Primitive {
    pub fn sign(&self) -> Option<Sign> {
        match self {
            Primitive::I8 | Primitive::I64 => Some(Sign::Signed),
            Primitive::U8 | Primitive::U64 => Some(Sign::Unsigned),
            Primitive::Unit | Primitive::Char | Primitive::Bool => None,
        }
    }

    pub fn parse(s: &str) -> Option<Primitive> {
        Some(match s {
            "()" => Primitive::Unit,
            "i8" => Primitive::I8,
            "i64" => Primitive::I64,
            "u8" => Primitive::U8,
            "u64" => Primitive::U64,
            "char" => Primitive::Char,
            "bool" => Primitive::Bool,
            _ => return None,
        })
    }
}

impl fmt::Display for Primitive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Primitive::Unit => "()",
            Primitive::I8 => "i8",
            Primitive::I64 => "i64",
            Primitive::U8 => "u8",
            Primitive::U64 => "u64",
            Primitive::Char => "char",
            Primitive::Bool => "bool",
        };

        write!(f, "{}", s)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParsedType<'s> {
    Primitive(Primitive),
    Pointer(Box<ParsedType<'s>>),
    Struct(&'s str),
}

impl<'s> ParsedType<'s> {
    pub fn sign(&self) -> Option<Sign> {
        if let ParsedType::Primitive(primitive) = self {
            primitive.sign()
        } else {
            None
        }
    }

    // pub fn can_cast_to(&self, other: &ParsedType) -> bool {
    //     use ParsedType::*;
    //
    //     matches!(
    //         (self, other),
    //         (Char, I64)
    //             | (I64, Char)
    //             | (Char, U8)
    //             | (U8, Char)
    //             | (Pointer(_), I64)
    //             | (Pointer(_), U64)
    //             | (I64, Pointer(_))
    //             | (U64, Pointer(_))
    //             | (Pointer(_), Pointer(_))
    //             | (I64, U64)
    //             | (U64, I64)
    //     )
    // }
}

impl<'s> From<&'s str> for ParsedType<'s> {
    fn from(string: &'s str) -> Self {
        if let Some(typ) = string.strip_suffix('*') {
            let typ = ParsedType::from(typ);
            return ParsedType::Pointer(Box::new(typ));
        }

        if let Some(primitive) = Primitive::parse(string) {
            return ParsedType::Primitive(primitive);
        }

        Self::Struct(string)
    }
}

impl<'s> std::fmt::Display for ParsedType<'s> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParsedType::Primitive(primitive) => write!(f, "{}", primitive),
            ParsedType::Pointer(typ) => write!(f, "*{}", typ),
            ParsedType::Struct(typ) => write!(f, "{}", typ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Sign {
    Signed,
    Unsigned,
}

pub struct Resolver<'e> {
    types: TypeMap,
    known_typeids: HashSet<TypeId>,
    known_functions: HashSet<u64>,

    err_ctx: &'e mut ErrorContext,
}

impl<'s, 'e> Resolver<'e> {
    pub fn new(err_ctx: &'e mut ErrorContext) -> Self {
        let types: HashMap<TypeId, TypeInfo> = [
            ("()", 0),
            ("u8", 1),
            ("i8", 1),
            ("char", 1),
            ("bool", 1),
            ("u64", 8),
            ("i64", 8),
        ]
        .into_iter()
        .map(|(name, size)| {
            (
                TypeId::from_parsed(&ParsedType::from(name)),
                TypeInfo {
                    size,
                    kind: TypeKind::Primitive(Primitive::parse(name).unwrap()),
                },
            )
        })
        .collect();

        let known_typeids = types.keys().copied().collect();

        Self {
            types: TypeMap { map: types },
            known_typeids,
            known_functions: HashSet::new(),
            err_ctx,
        }
    }

    pub fn resolve_and_combine(
        mut self,
        mut ast_vec: Vec<AST<'s, (ParsedType<'s>, Span)>>,
    ) -> (AST<'s, TypeId>, TypeMap) {
        for item in ast_vec.iter_mut().flat_map(|ast| ast.items.iter_mut()) {
            match item {
                Item::Struct { name, .. } => {
                    let typeid = TypeId::from_parsed(&ParsedType::Struct(name));
                    self.known_typeids.insert(typeid);
                }
                Item::Function(FnDef { name, .. }) | Item::ForwardDecl { name, .. } => {
                    let mut h = FxHasher::default();
                    name.hash(&mut h);
                    self.known_functions.insert(h.finish());
                }
                Item::Impl {
                    struct_name,
                    functions,
                } => {
                    for def in functions {
                        let name = format!("{}::{}", struct_name, def.name);
                        let mut h = FxHasher::default();
                        name.hash(&mut h);
                        self.known_functions.insert(h.finish());
                    }
                }
                _ => {}
            }
        }

        let mut main_ast = AST::new();
        let sum_items = ast_vec.iter().map(|a| a.items.len()).sum();
        main_ast.items.reserve_exact(sum_items);

        for ast in ast_vec.into_iter() {
            let AST {
                mangled_path,
                imports,
                items,
                ..
            } = ast;

            let mangled_path = mangled_path.unwrap();

            for (import, span) in imports.iter() {
                let typeid = TypeId::from_parsed(&ParsedType::Struct(import));
                if self.known_typeids.contains(&typeid) {
                    continue;
                }

                let mut h = FxHasher::default();
                import.hash(&mut h);
                let hash = h.finish();
                if self.known_functions.contains(&hash) {
                    continue;
                }

                self.err_ctx
                    .error(span.clone())
                    .with_message("unknown import")
                    .with_label(span.clone(), "could not find item")
                    .report();
            }

            for item in items.into_iter() {
                let item = match item {
                    Item::Function(fn_def) => {
                        Item::Function(self.function(&mangled_path, &imports, fn_def))
                    }
                    Item::ForwardDecl {
                        name,
                        args,
                        ret_type: (ret_type, ret_type_span),
                        decl_span,
                    } => Item::ForwardDecl {
                        name,
                        args: args
                            .into_iter()
                            .map(|(name, (typ, typ_span), arg_span)| {
                                (
                                    name,
                                    self.resolve_type(&mangled_path, &imports, typ, typ_span),
                                    arg_span,
                                )
                            })
                            .collect(),
                        ret_type: self.resolve_type(
                            &mangled_path,
                            &imports,
                            ret_type,
                            ret_type_span,
                        ),
                        decl_span,
                    },
                    Item::ExternLib(_) => todo!(),
                    Item::MemorySegment {
                        name,
                        typ: (typ, span),
                    } => Item::MemorySegment {
                        name,
                        typ: self.resolve_type(&mangled_path, &imports, typ, span),
                    },
                    Item::Struct {
                        name,
                        decl_span,
                        fields,
                    } => {
                        let fields = fields
                            .into_iter()
                            .map(|(name, (typ, typ_span), decl_span)| {
                                (
                                    name,
                                    self.resolve_type(&mangled_path, &imports, typ, typ_span),
                                    decl_span,
                                )
                            })
                            .collect();

                        Item::Struct {
                            name,
                            decl_span,
                            fields,
                        }
                    }
                    Item::Impl {
                        struct_name,
                        functions,
                    } => Item::Impl {
                        struct_name,
                        functions: functions
                            .into_iter()
                            .map(|def| self.function(&mangled_path, &imports, def))
                            .collect(),
                    },
                };

                main_ast.items.push(item);
            }
        }

        let mut struct_fields = HashMap::new();
        for item in main_ast.items.iter() {
            if let Item::Struct { name, fields, .. } = item {
                let typeid = TypeId::from_parsed(&ParsedType::Struct(name));
                struct_fields.insert(
                    typeid,
                    (
                        name.clone().into_owned(),
                        fields
                            .iter()
                            .map(|(name, typeid, span)| {
                                ((*name).to_owned(), *typeid, 0, span.clone())
                            })
                            .collect(),
                    ),
                );
            }
        }

        for typeid in self.known_typeids {
            struct_def_size(&mut self.types, typeid, &mut struct_fields);
        }

        (main_ast, self.types)
    }

    fn function(
        &mut self,
        mangled_path: &str,
        imports: &[(&str, Span)],
        def: FnDef<'s, (ParsedType<'s>, Span)>,
    ) -> FnDef<'s, TypeId> {
        let FnDef {
            name,
            args,
            body,
            decl_span,
            ret_type: (ret_type, ret_type_span),
        } = def;

        let args = args
            .into_iter()
            .map(|(name, (typ, typ_span), decl_span)| {
                (
                    name,
                    self.resolve_type(mangled_path, imports, typ, typ_span),
                    decl_span,
                )
            })
            .collect();

        let body = self.body(mangled_path, imports, body);

        let ret_type = self.resolve_type(mangled_path, imports, ret_type, ret_type_span);

        FnDef {
            name,
            args,
            body,
            decl_span,
            ret_type,
        }
    }

    fn body(
        &mut self,
        mangled_path: &str,
        imports: &[(&str, Span)],
        body: Vec<Statement<'s, (ParsedType<'s>, Span)>>,
    ) -> Vec<Statement<'s, TypeId>> {
        body.into_iter()
            .map(|stmt| self.statement(mangled_path, imports, stmt))
            .collect()
    }

    fn statement(
        &mut self,
        mangled_path: &str,
        imports: &[(&str, Span)],
        stmt: Statement<'s, (ParsedType<'s>, Span)>,
    ) -> Statement<'s, TypeId> {
        match stmt {
            Statement::Declare {
                var,
                expr,
                var_span,
                explicit_type,
            } => Statement::Declare {
                var,
                expr: self.expression(mangled_path, imports, expr),
                var_span,
                explicit_type: explicit_type
                    .map(|(typ, span)| self.resolve_type(mangled_path, imports, typ, span)),
            },
            Statement::Assign {
                var,
                expr,
                var_span,
            } => Statement::Assign {
                var: self.var_assign(mangled_path, imports, var),
                expr: self.expression(mangled_path, imports, expr),
                var_span,
            },
            Statement::If { guard, body } => Statement::If {
                guard: self.expression(mangled_path, imports, guard),
                body: self.body(mangled_path, imports, body),
            },
            Statement::Return(expr) => {
                Statement::Return(self.expression(mangled_path, imports, expr))
            }
            Statement::Expr(expr) => Statement::Expr(self.expression(mangled_path, imports, expr)),
            Statement::WhileLoop { guard, body } => Statement::WhileLoop {
                guard: self.expression(mangled_path, imports, guard),
                body: self.body(mangled_path, imports, body),
            },
        }
    }

    fn var_assign(
        &mut self,
        mangled_path: &str,
        imports: &[(&str, Span)],
        var: Assignable<'s, (ParsedType<'s>, Span)>,
    ) -> Assignable<'s, TypeId> {
        match var {
            Assignable::Var(name) => Assignable::Var(name),
            Assignable::Ptr(name, val_size) => Assignable::Ptr(name, val_size),
            Assignable::Index {
                data,
                index,
                val_size,
            } => Assignable::Index {
                data: Box::new(self.expression(mangled_path, imports, *data)),
                index: Box::new(self.expression(mangled_path, imports, *index)),
                val_size,
            },
            Assignable::MemberAccess(expr, member) => Assignable::MemberAccess(
                Box::new(self.expression(mangled_path, imports, *expr)),
                member,
            ),
        }
    }

    fn expression(
        &mut self,
        mangled_path: &str,
        imports: &[(&str, Span)],
        expr: Expression<'s, (ParsedType<'s>, Span)>,
    ) -> Expression<'s, TypeId> {
        let Expression { inner, typ, span } = expr;

        let typ = typ.map(|(typ, span)| self.resolve_type(mangled_path, imports, typ, span));

        let inner = match inner {
            ExprInner::Const(num, typ) => ExprInner::Const(num, typ),
            ExprInner::Character(ch) => ExprInner::Character(ch),
            ExprInner::String(string) => ExprInner::String(string),
            ExprInner::Bool(boo) => ExprInner::Bool(boo),
            ExprInner::Ident(var) => ExprInner::Ident(var),
            ExprInner::Pointer(var) => ExprInner::Pointer(var),
            ExprInner::Deref(var, _) => ExprInner::Deref(var, None),
            ExprInner::Arithmetic(lhs, rhs, arithmetic_op, sign) => ExprInner::Arithmetic(
                Box::new(self.expression(mangled_path, imports, *lhs)),
                Box::new(self.expression(mangled_path, imports, *rhs)),
                arithmetic_op,
                sign,
            ),
            ExprInner::Comparison(lhs, rhs, compare_op, sign) => ExprInner::Comparison(
                Box::new(self.expression(mangled_path, imports, *lhs)),
                Box::new(self.expression(mangled_path, imports, *rhs)),
                compare_op,
                sign,
            ),
            ExprInner::Logical(lhs, rhs, logical_op) => ExprInner::Logical(
                Box::new(self.expression(mangled_path, imports, *lhs)),
                Box::new(self.expression(mangled_path, imports, *rhs)),
                logical_op,
            ),
            ExprInner::Not(expr) => {
                ExprInner::Not(Box::new(self.expression(mangled_path, imports, *expr)))
            }
            ExprInner::Negate(expr) => {
                ExprInner::Negate(Box::new(self.expression(mangled_path, imports, *expr)))
            }
            ExprInner::Cast(expr, (typ, span)) => ExprInner::Cast(
                Box::new(self.expression(mangled_path, imports, *expr)),
                self.resolve_type(mangled_path, imports, typ, span),
            ),
            ExprInner::Index {
                data,
                index,
                val_size,
            } => ExprInner::Index {
                data: Box::new(self.expression(mangled_path, imports, *data)),
                index: Box::new(self.expression(mangled_path, imports, *index)),
                val_size,
            },
            ExprInner::MemberAccess(expr, member, type_id) => ExprInner::MemberAccess(
                Box::new(self.expression(mangled_path, imports, *expr)),
                member,
                type_id,
            ),
            ExprInner::FnCall(fn_name, exprs) => ExprInner::FnCall(
                self.resolve_function(mangled_path, imports, fn_name, span.clone()),
                exprs
                    .into_iter()
                    .map(|expr| self.expression(mangled_path, imports, expr))
                    .collect(),
            ),
            ExprInner::Construct {
                typ: (typ, span),
                fields,
            } => ExprInner::Construct {
                typ: self.resolve_type(mangled_path, imports, typ, span),
                fields: fields
                    .into_iter()
                    .map(|(name, expr)| (name, self.expression(mangled_path, imports, expr)))
                    .collect(),
            },
            ExprInner::SizeOf((typ, span)) => {
                ExprInner::SizeOf(self.resolve_type(mangled_path, imports, typ, span))
            }
        };

        Expression { inner, typ, span }
    }

    fn resolve_function(
        &mut self,
        mangled_path: &str,
        imports: &[(&str, Span)],
        fnptr: FnPtr<'s, (ParsedType<'s>, Span)>,
        span: Span,
    ) -> FnPtr<'s, TypeId> {
        match fnptr {
            FnPtr::Named(path) => {
                panic!()
            }
            FnPtr::Expr(expr) => {
                let expr = self.expression(mangled_path, imports, *expr);

                match expr {
                    // Expression {
                    //     inner: ExprInner::MemberAccess(parent, member, member_type),
                    //     typ,
                    //     span,
                    // } => {
                    //     let parent = self.expression(mangled_path, imports, parent);
                    //     if let Some(parent_type) = parent.typ {
                    //         let typeinfo = self.types.get(parent_type);
                    //     }
                    // }
                    Expression {
                        inner: ExprInner::Ident(path),
                        typ: _,
                        span,
                    } => {
                        let local_function = format!("{}::{}", mangled_path, path);
                        let mut h = FxHasher::default();
                        local_function.hash(&mut h);
                        let local_hash = h.finish();
                        if self.known_functions.contains(&local_hash) {
                            return FnPtr::Named(Cow::Owned(local_function));
                        }

                        let mut h = FxHasher::default();
                        path.hash(&mut h);
                        let absolute_hash = h.finish();
                        if self.known_functions.contains(&absolute_hash) {
                            return FnPtr::Named(Cow::Borrowed(path));
                        }

                        let root = path
                            .split_once("::")
                            .map(|(l, _)| l)
                            .unwrap_or(path.as_ref());

                        for (import, _) in imports {
                            if let Some((prepath, name)) = import.rsplit_once("::")
                                && name == root
                            {
                                return FnPtr::Named(Cow::Owned(format!("{}::{}", prepath, path)));
                            }
                        }

                        self.err_ctx
                            .error(span.clone())
                            .with_message("unknown function")
                            .with_label(span, "please find this...")
                            .report();

                        FnPtr::Named(Cow::Borrowed(path))
                    }
                    _ => {
                        // self.err_ctx
                        //     .error(expr.span.clone())
                        //     .with_message("unable to locate function")
                        //     .with_label(
                        //         expr.span.clone(),
                        //         "cannot find function related to this expression",
                        //     )
                        //     .report();

                        println!("{:?}", expr);

                        FnPtr::Expr(Box::new(expr))
                    }
                }
            }
        }
    }

    fn resolve_type(
        &mut self,
        mangled_path: &str,
        imports: &[(&str, Span)],
        typ: ParsedType<'s>,
        span: Span,
    ) -> TypeId {
        match typ {
            ParsedType::Primitive(_) => TypeId::from_parsed(&typ),
            ParsedType::Pointer(value_type) => {
                let value_typeid = self.resolve_type(mangled_path, imports, *value_type, span);
                self.types.ptr_type_to(value_typeid)
            }
            ParsedType::Struct(typename) => {
                let local_typepath = format!("{}::{}", mangled_path, typename);

                let local_typeid = TypeId::from_parsed(&ParsedType::Struct(&local_typepath));
                if self.known_typeids.contains(&local_typeid) {
                    return local_typeid;
                }

                let absolute_typeid = TypeId::from_parsed(&ParsedType::Struct(typename));
                if self.known_typeids.contains(&absolute_typeid) {
                    return absolute_typeid;
                }

                for (import, _) in imports {
                    if matches!(import.rsplit_once("::"), Some((_, typ)) if typ == typename) {
                        let import_typeid = TypeId::from_parsed(&ParsedType::Struct(import));

                        assert!(self.known_typeids.contains(&import_typeid));

                        return import_typeid;
                    }
                }

                self.err_ctx
                    .error(span.clone())
                    .with_message(format!("unknown type {}", typename))
                    .with_label(span, "what is this?")
                    .report();

                TypeId(0)
                // panic!("type {} does not exist as '{}'", typename, local_typepath)
            }
        }
    }
}

fn struct_def_size(
    types: &mut TypeMap,
    typeid: TypeId,
    struct_fields: &mut HashMap<TypeId, (String, Vec<(String, TypeId, u64, Span)>)>,
) -> u64 {
    if let Some(type_info) = types.map.get(&typeid) {
        return type_info.size;
    }

    let mut size = 0;
    let (qualifier, mut fields) = struct_fields.remove(&typeid).unwrap();

    for (_, field_type, field_offset, _) in fields.iter_mut() {
        let field_size = struct_def_size(types, *field_type, struct_fields);

        let oversize = size % field_size.clamp(1, 8);
        if oversize > 0 {
            size += field_size.min(8) - oversize;
        }

        *field_offset = size;
        size += field_size;
    }

    types.map.insert(
        typeid,
        TypeInfo {
            size,
            kind: TypeKind::Struct {
                qualifier: qualifier.to_owned(),
                fields,
            },
        },
    );

    size
}
