use std::{
    collections::{HashMap, HashSet},
    fmt,
    hash::{Hash, Hasher},
    sync::OnceLock,
};

use rustc_hash::FxHasher;

use crate::analyze::{
    ErrorContext, Span,
    ast::{AST, Assignable, ExprInner, Expression, FnDef, Item, Statement},
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
        fields: Vec<(String, TypeId, u64)>,
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
            for (field_name, _, offset) in fields.iter() {
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

    pub fn from_str(s: &str) -> Option<Primitive> {
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

        if let Some(primitive) = Primitive::from_str(string) {
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
                    kind: TypeKind::Primitive(Primitive::from_str(name).unwrap()),
                },
            )
        })
        .collect();

        let known_typeids = types.keys().copied().collect();

        Self {
            types: TypeMap { map: types },
            known_typeids,
            err_ctx,
        }
    }

    pub fn resolve_and_combine(
        mut self,
        mut ast_vec: Vec<AST<'s, (ParsedType<'s>, Span)>>,
    ) -> (AST<'s, TypeId>, TypeMap) {
        for ast in ast_vec.iter_mut() {
            let mangled_path = ast
                .mangled_path
                .as_ref()
                .expect("AST was not mangled before type resolution");

            for item in ast.items.iter_mut() {
                if let Item::Struct { name, .. } = item {
                    let typeid = TypeId::from_parsed(&ParsedType::Struct(name));
                    println!("known typeid {} => {:?}", name, typeid);
                    self.known_typeids.insert(typeid);
                }
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

        let mut struct_fields: HashMap<TypeId, (String, Vec<(String, TypeId, u64)>)> =
            HashMap::new();
        for item in main_ast.items.iter() {
            if let Item::Struct { name, fields, .. } = item {
                let typeid = TypeId::from_parsed(&ParsedType::Struct(name));
                struct_fields.insert(
                    typeid,
                    (
                        name.clone().into_owned(),
                        fields
                            .iter()
                            .map(|(name, typeid, _)| ((*name).to_owned(), *typeid, 0))
                            .collect(),
                    ),
                );
            }
        }

        for typeid in self.known_typeids {
            struct_def_size(&mut self.types, typeid, &mut struct_fields);
        }

        for (typeid, typeinfo) in self.types.map.iter() {
            println!(
                "sizeof({}) = {}",
                self.types.display(*typeid),
                typeinfo.size
            );
        }

        (main_ast, self.types)
    }

    fn function(
        &mut self,
        mangled_path: &str,
        imports: &[&str],
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
        imports: &[&str],
        body: Vec<Statement<'s, (ParsedType<'s>, Span)>>,
    ) -> Vec<Statement<'s, TypeId>> {
        body.into_iter()
            .map(|stmt| self.statement(mangled_path, imports, stmt))
            .collect()
    }

    fn statement(
        &mut self,
        mangled_path: &str,
        imports: &[&str],
        stmt: Statement<'s, (ParsedType<'s>, Span)>,
    ) -> Statement<'s, TypeId> {
        match stmt {
            Statement::Declare {
                var,
                expr,
                var_span,
            } => Statement::Declare {
                var,
                expr: self.expression(mangled_path, imports, expr),
                var_span,
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
        imports: &[&str],
        var: Assignable<'s, (ParsedType<'s>, Span)>,
    ) -> Assignable<'s, TypeId> {
        match var {
            Assignable::Var(name) => Assignable::Var(name),
            Assignable::Ptr(name, val_size) => Assignable::Ptr(name, val_size),
            Assignable::Index(name, expr, val_size) => Assignable::Index(
                name,
                Box::new(self.expression(mangled_path, imports, *expr)),
                val_size,
            ),
            Assignable::MemberAccess(expr, member) => Assignable::MemberAccess(
                Box::new(self.expression(mangled_path, imports, *expr)),
                member,
            ),
        }
    }

    fn expression(
        &mut self,
        mangled_path: &str,
        imports: &[&str],
        expr: Expression<'s, (ParsedType<'s>, Span)>,
    ) -> Expression<'s, TypeId> {
        let Expression { inner, typ, span } = expr;

        let typ = typ.map(|(typ, span)| self.resolve_type(mangled_path, imports, typ, span));

        let inner = match inner {
            ExprInner::Const(num, typ) => ExprInner::Const(num, typ),
            ExprInner::Character(ch) => ExprInner::Character(ch),
            ExprInner::String(string) => ExprInner::String(string),
            ExprInner::Bool(boo) => ExprInner::Bool(boo),
            ExprInner::Variable(var) => ExprInner::Variable(var),
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
            ExprInner::Index(var, expr, val_size) => ExprInner::Index(
                var,
                Box::new(self.expression(mangled_path, imports, *expr)),
                val_size,
            ),
            ExprInner::MemberAccess(expr, member, type_id) => ExprInner::MemberAccess(
                Box::new(self.expression(mangled_path, imports, *expr)),
                member,
                type_id,
            ),
            ExprInner::FnCall(fn_name, exprs) => ExprInner::FnCall(
                fn_name,
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

    fn resolve_type(
        &mut self,
        mangled_path: &str,
        imports: &[&str],
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
                println!("\n- Resolve '{}'", typename);
                let local_typepath = format!("{}::{}", mangled_path, typename);
                println!("local: {}", local_typepath);

                let local_typeid = TypeId::from_parsed(&ParsedType::Struct(&local_typepath));
                if self.known_typeids.contains(&local_typeid) {
                    return local_typeid;
                }

                let absolute_typeid = TypeId::from_parsed(&ParsedType::Struct(typename));
                println!("absolute: {}", typename);
                if self.known_typeids.contains(&absolute_typeid) {
                    return absolute_typeid;
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
    struct_fields: &mut HashMap<TypeId, (String, Vec<(String, TypeId, u64)>)>,
) -> u64 {
    if let Some(type_info) = types.map.get(&typeid) {
        return type_info.size;
    }

    let mut size = 0;
    let (qualifier, mut fields) = struct_fields.remove(&typeid).unwrap();

    for (_, field_type, field_offset) in fields.iter_mut() {
        let field_size = struct_def_size(types, *field_type, struct_fields);

        let oversize = size % dbg!(field_size.clamp(1, 8));
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
