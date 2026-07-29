use std::{
    collections::{HashMap, HashSet},
    fmt,
};

use crate::{
    analyze::{
        ErrorContext, ErrorVec, Span,
        ast::{AST, Assignable, ExprInner, Expression, FnDef, Item, Statement},
    },
    ir::ValSize,
};

pub mod nameres;

pub struct ValidAST(pub AST);

pub fn analyze(mut ast: AST, main_fn: impl Into<String>) -> Result<(ValidAST, Analyzer), ErrorVec> {
    let mut analyzer = Analyzer::new(main_fn);
    analyzer.analyze(&mut ast)?;

    Ok((ValidAST(ast), analyzer))
}

pub struct Analyzer {
    err_ctx: ErrorContext,

    main_fn: String,
    variables: HashMap<String, SemanticType>,
    globals: HashMap<String, SemanticType>,
    functions: HashMap<String, (Span, SemanticType, Vec<(Span, SemanticType)>)>,
    function_calls: HashMap<String, HashSet<String>>,
    fn_call_context: HashSet<String>,
    struct_defs: HashMap<String, Vec<(String, SemanticType, Span)>>,
    types: HashMap<String, DataType>,
}

impl Analyzer {
    pub fn new(main_fn: impl Into<String>) -> Self {
        Self {
            err_ctx: ErrorContext::new(),

            main_fn: main_fn.into(),
            variables: HashMap::new(),
            globals: HashMap::new(),
            functions: HashMap::new(),
            function_calls: HashMap::new(),
            fn_call_context: HashSet::new(),
            struct_defs: HashMap::new(),
            types: HashMap::new(),
        }
    }

    pub fn analyze(&mut self, ast: &mut AST) -> Result<(), ErrorVec> {
        for item in &ast.items {
            match item {
                Item::Function(FnDef {
                    name,
                    ret_type,
                    decl_span,
                    args,
                    ..
                })
                | Item::ForwardDecl {
                    name,
                    ret_type,
                    decl_span,
                    args,
                } => {
                    let args = args
                        .iter()
                        .map(|(_, typ, span)| (span.clone(), typ.clone()))
                        .collect();

                    if let Some((other_decl_span, _, _)) = self.functions.insert(
                        name.to_owned(),
                        (decl_span.clone(), ret_type.to_owned(), args),
                    ) {
                        self.err_ctx
                            .error(decl_span.clone())
                            .with_message("duplicate function definition")
                            .with_label(decl_span.clone(), "defined here")
                            .with_label(other_decl_span.clone(), "first defined here")
                            .report();
                    }
                }
                Item::Impl {
                    struct_name,
                    functions,
                } => {
                    for function in functions {
                        let FnDef {
                            name,
                            ret_type,
                            decl_span,
                            args,
                            ..
                        } = function;

                        let args = args
                            .iter()
                            .map(|(_, typ, span)| (span.clone(), typ.clone()))
                            .collect();

                        if let Some((other_decl_span, _, _)) = self.functions.insert(
                            format!("{}::{}", struct_name, name),
                            (decl_span.clone(), ret_type.to_owned(), args),
                        ) {
                            self.err_ctx
                                .error(decl_span.clone())
                                .with_message("duplicate function definition")
                                .with_label(decl_span.clone(), "defined here")
                                .with_label(other_decl_span.clone(), "first defined here")
                                .report();
                        }
                    }
                }
                Item::ExternLib(_) => {}
                Item::MemorySegment { name, typ } => {
                    self.globals.insert(name.clone(), typ.clone());
                }
                Item::Struct {
                    name,
                    decl_span,
                    fields,
                } => {
                    self.struct_defs.insert(name.to_owned(), fields.to_owned());
                }
            }
        }

        self.process_struct_defs();

        for item in &mut ast.items {
            self.item(item);
        }

        let mut used_functions: HashSet<String> = HashSet::new();
        used_functions.insert(self.main_fn.clone());
        let mut queue: Vec<&str> = vec![&self.main_fn];

        while let Some(&func) = queue.first() {
            if let Some(iter) = self.function_calls.get(func) {
                for callee in iter {
                    if !used_functions.contains(callee) {
                        used_functions.insert(callee.to_owned());
                        queue.push(callee);
                    }
                }
            }

            queue.remove(0);
        }

        ast.items.retain_mut(|item| match item {
            Item::Function(FnDef { name, .. }) => used_functions.contains(name),
            Item::Impl {
                struct_name,
                functions,
            } => {
                functions.retain(|fndef| {
                    used_functions.contains(&format!("{}::{}", struct_name, fndef.name))
                });

                true
            }
            Item::Struct { .. } | Item::MemorySegment { .. } => true,
            Item::ForwardDecl { .. } | Item::ExternLib(_) => false,
        });

        // for item in &ast.items {
        //     if let Item::Function {
        //         name, decl_span, ..
        //     } = item
        //         && name != MAIN_FN
        //         && !self.called_funcs.contains(name)
        //     {
        //         // self.err_ctx
        //         //     .warn(decl_span.clone())
        //         //     .with_message("unused function")
        //         //     .with_label(decl_span.clone(), "function is never used")
        //         //     .report();
        //     }
        // }

        if !self.err_ctx.is_empty() {
            return Err(self.err_ctx.take_errors());
        }

        Ok(())
    }

    fn cache_definitions(&mut self, ast_map: &Vec<AST>) {}

    fn process_struct_defs(&mut self) {
        for name in self.struct_defs.keys().cloned().collect::<Vec<_>>() {
            self.struct_def_size(&SemanticType::UserType(name));
        }
    }

    fn struct_def_size(&mut self, typ: &SemanticType) -> u64 {
        match typ {
            SemanticType::Unit => 0,
            SemanticType::I8 | SemanticType::U8 | SemanticType::Bool | SemanticType::Char => 1,
            SemanticType::I64 | SemanticType::U64 => 8,
            SemanticType::Pointer(_) => 8,
            SemanticType::UserType(name) => {
                if let Some(datatype) = self.types.get(name) {
                    return datatype.size;
                }

                let mut size = 0;
                let mut fields = self
                    .struct_defs
                    .get(name)
                    .unwrap()
                    .iter()
                    .cloned()
                    .map(|(n, t, s)| (n, t, 0))
                    .collect::<Vec<_>>();

                for (_, field_type, field_offset) in fields.iter_mut() {
                    let field_size = self.struct_def_size(field_type);

                    let oversize = size % field_size.min(8);
                    if oversize > 0 {
                        size += field_size.min(8) - oversize;
                    }

                    *field_offset = size;
                    size += field_size;
                }

                self.types
                    .insert(name.to_owned(), DataType { fields, size });

                size
            }
        }
    }

    pub fn size_of(&self, typ: &SemanticType) -> u64 {
        match typ {
            SemanticType::Unit => 0,
            SemanticType::Bool | SemanticType::Char | SemanticType::I8 | SemanticType::U8 => 1,
            SemanticType::I64 | SemanticType::U64 => 8,
            SemanticType::Pointer(_) => 8,
            SemanticType::UserType(name) => {
                self.types.get(name).map(|t| t.size).unwrap_or_else(|| 0)
            }
        }
    }

    pub fn offset_of_member(&self, typename: &str, member: &str) -> u64 {
        let typ = self.types.get(typename).unwrap();
        for (field_name, _, offset) in typ.fields.iter() {
            if field_name == member {
                return *offset;
            }
        }

        panic!("unknown member")
    }

    fn item(&mut self, item: &mut Item) {
        self.variables.clear();

        match item {
            Item::Function(FnDef {
                name,
                args,
                body,
                decl_span,
                ret_type,
                ret_type_span,
            }) => {
                self.verify_type(ret_type, ret_type_span);

                for (arg, typ, _) in args {
                    self.verify_type(typ, decl_span);
                    self.variables.insert(arg.to_owned(), typ.clone());
                }

                let has_return = self.body(body, ret_type, decl_span);

                if !has_return && (name == &self.main_fn || ret_type != &SemanticType::Unit) {
                    self.err_ctx
                        .error(decl_span.clone())
                        .with_message("no return statement found in function main")
                        .with_label(decl_span.clone(), "main must return a value")
                        .report();
                }

                let calls = std::mem::take(&mut self.fn_call_context);
                self.function_calls.insert(name.to_owned(), calls);
            }
            Item::Impl {
                struct_name,
                functions,
            } => {
                for fndef in functions {
                    let FnDef {
                        name,
                        args,
                        body,
                        decl_span,
                        ret_type,
                        ret_type_span,
                    } = fndef;

                    let name = format!("{}::{}", struct_name, name);

                    self.verify_type(ret_type, ret_type_span);

                    for (arg, typ, _) in args {
                        self.verify_type(typ, decl_span);
                        self.variables.insert(arg.to_owned(), typ.clone());
                    }

                    let has_return = self.body(body, ret_type, decl_span);

                    if !has_return && (name == self.main_fn || ret_type != &SemanticType::Unit) {
                        self.err_ctx
                            .error(decl_span.clone())
                            .with_message("no return statement found in function")
                            .with_label(decl_span.clone(), "function must return a value")
                            .report();
                    }

                    let calls = std::mem::take(&mut self.fn_call_context);
                    self.function_calls.insert(name.to_owned(), calls);
                }
            }
            Item::ForwardDecl { .. } => {}
            Item::ExternLib(_lib) => (), // TODO: maybe?
            Item::MemorySegment { .. } => {}
            Item::Struct {
                name,
                decl_span,
                fields,
            } => {
                for (_, typ, field_span) in fields {
                    self.verify_type(typ, field_span);
                }
            }
        }
    }

    /// Returns whether this statement contains a return statement
    fn body(
        &mut self,
        body: &mut [Statement],
        fn_ret_type: &SemanticType,
        fn_decl_span: &Span,
    ) -> bool {
        let mut has_return = false;
        for stmt in body {
            if self.statement(stmt, fn_ret_type, fn_decl_span) {
                has_return = true;
            }
        }

        has_return
    }

    /// Returns whether this statement contains a return statement
    fn statement(
        &mut self,
        stmt: &mut Statement,
        fn_ret_type: &SemanticType,
        fn_decl_span: &Span,
    ) -> bool {
        match stmt {
            Statement::Declare {
                var,
                expr,
                var_span,
            } => {
                let var_type = self.expression(expr, None);

                if self
                    .variables
                    .insert(var.clone(), var_type.unwrap_or(SemanticType::Unit))
                    .is_some()
                {
                    self.err_ctx
                        .error(var_span.clone())
                        .with_message("duplicate variable declaration")
                        .with_label(var_span.clone(), "variable already defined")
                        .report();
                }
            }
            Statement::Assign {
                var,
                expr,
                var_span,
            } => {
                let assign_type = self.expression(expr, None);

                let decl_type = match var {
                    Assignable::Var(var) => self.check_var(var, var_span),
                    Assignable::Ptr(ptr, size) => {
                        let typ = self.check_ptr(ptr, var_span);
                        if let Some(typ) = typ.as_ref() {
                            *size = Some(ValSize::from_bytes(self.size_of(typ)).unwrap());
                        }

                        typ
                    }
                    Assignable::Index(array, index, size) => {
                        let item_type = self.check_index(array, index, var_span);

                        if let Some(item_type) = item_type {
                            *size = Some(ValSize::from_bytes(self.size_of(&item_type)).unwrap());
                        }

                        None
                    }
                    Assignable::MemberAccess(parent, member) => {
                        match self.expression(parent, None) {
                            Some(SemanticType::Pointer(
                                ref user_type @ deref!(SemanticType::UserType(ref name)),
                            )) => {
                                if self.verify_type(user_type, var_span) {
                                    let data_type = self.types.get(name).unwrap();
                                    let field_type =
                                        data_type.fields.iter().find_map(|(n, t, _)| {
                                            if n == member { Some(t.clone()) } else { None }
                                        });

                                    if field_type.is_none() {
                                        self.err_ctx
                                            .error(var_span.clone())
                                            .with_message("invalid member access")
                                            .with_label(
                                                var_span.clone(),
                                                format!("type {} has no member {}", name, member),
                                            )
                                            .report();
                                    }

                                    field_type
                                } else {
                                    None
                                }
                            }
                            Some(typ) => {
                                self.verify_type(&typ, var_span);
                                self.err_ctx
                                    .error(var_span.clone())
                                    .with_message("invalid member access")
                                    .with_label(
                                        var_span.clone(),
                                        format!("type {} has no members", typ),
                                    )
                                    .report();

                                None
                            }
                            None => None,
                        }
                    }
                };

                if let Some(assign_type) = assign_type
                    && let Some(decl_type) = decl_type
                    && assign_type != decl_type
                {
                    self.err_ctx
                        .error(combine_span(var_span, &expr.span))
                        .with_message("mismatched types")
                        .with_label(var_span.clone(), format!("this is of type {}", decl_type))
                        .with_label(
                            expr.span.clone(),
                            format!("this is of type {}", assign_type),
                        )
                        .report();
                }
            }
            Statement::If { guard, body } | Statement::WhileLoop { guard, body } => {
                if let Some(typ) = self.expression(guard, Some(&SemanticType::Bool))
                    && typ != SemanticType::Bool
                {
                    self.err_ctx
                        .error(guard.span.clone())
                        .with_message("unexpected type")
                        .with_label(
                            guard.span.clone(),
                            format!("expected type 'bool', got '{}'", typ),
                        )
                        .report();
                }

                return self.body(body, fn_ret_type, fn_decl_span);
            }
            Statement::Expr(expr) => {
                self.expression(expr, None);
            }
            Statement::Return(expr) => {
                if let Some(typ) = self.expression(expr, Some(fn_ret_type))
                    && &typ != fn_ret_type
                {
                    self.err_ctx
                        .error(expr.span.clone())
                        .with_message("incompatible types")
                        .with_label(expr.span.clone(), format!("this is of type {}", typ))
                        .with_label(
                            fn_decl_span.clone(),
                            format!("function returns {}", fn_ret_type),
                        )
                        .report();
                }

                return true;
            }
        }

        false
    }

    fn expression(
        &mut self,
        expr: &mut Expression,
        hint: Option<&SemanticType>,
    ) -> Option<SemanticType> {
        let typ = match &mut expr.inner {
            ExprInner::Const(_, explicit_type) => Some(
                explicit_type
                    .clone()
                    .or_else(|| {
                        hint.filter(|hint| hint.compatible_with(&SemanticType::I64))
                            .cloned()
                    })
                    .unwrap_or(SemanticType::I64),
            ),
            ExprInner::Character(_) => Some(SemanticType::Char),
            ExprInner::String(_) => Some(SemanticType::Pointer(Box::new(SemanticType::Char))),
            ExprInner::Bool(_) => Some(SemanticType::Bool),

            ExprInner::Variable(var) => self.check_var(var, &expr.span),
            ExprInner::Pointer(var) => self
                .check_var(var, &expr.span)
                .map(|t| SemanticType::Pointer(Box::new(t))),
            ExprInner::Deref(var, typ) => {
                *typ = self.check_ptr(var, &expr.span);
                typ.clone()
            }

            ExprInner::Arithmetic(expr1, expr2, _op, expr_sign) => {
                if let Some(type1) = self.expression(expr1, hint)
                    && let Some(type2) = self.expression(expr2, Some(&type1))
                {
                    if type1 == type2 {
                        if let Some(type_sign) = type1.sign() {
                            *expr_sign = Some(type_sign);
                            expr.typ = Some(type1.clone());
                            return Some(type1);
                        }

                        self.err_ctx
                            .error(combine_span(&expr1.span, &expr2.span))
                            .with_message("mismatched arithmetic types")
                            .with_label(
                                expr1.span.clone(),
                                "arithmetic only allowed on integer types",
                            )
                            .report();
                    }

                    self.err_ctx
                        .error(combine_span(&expr1.span, &expr2.span))
                        .with_message("mismatched types")
                        .with_label(expr1.span.clone(), format!("this is of type {}", type1))
                        .with_label(expr2.span.clone(), format!("this is of type {}", type2))
                        .report();
                }

                None
            }

            ExprInner::Comparison(expr1, expr2, _op, expr_sign) => {
                if let Some(type1) = self.expression(expr1, None)
                    && let Some(type2) = self.expression(expr2, None)
                {
                    if type1 == type2 {
                        let sign1 = type1.sign();
                        let sign2 = type2.sign();
                        if sign1 == sign2 {
                            *expr_sign = sign1;
                            expr.typ = Some(type1.clone());
                            return Some(SemanticType::Bool);
                        }

                        let sign1_str = match sign1 {
                            Some(Sign::Signed) => "a signed integer",
                            Some(Sign::Unsigned) => "an unsigned integer",
                            None => "not an integer",
                        };

                        let sign2_str = match sign2 {
                            Some(Sign::Signed) => "a signed integer",
                            Some(Sign::Unsigned) => "an unsigned integer",
                            None => "not an integer",
                        };

                        self.err_ctx
                            .error(combine_span(&expr1.span, &expr2.span))
                            .with_message(
                                "mismatched comparison types, must have same sign/no sign",
                            )
                            .with_label(expr1.span.clone(), format!("this is {}", sign1_str))
                            .with_label(expr2.span.clone(), format!("this is {}", sign2_str))
                            .report();
                    }

                    self.err_ctx
                        .error(combine_span(&expr1.span, &expr2.span))
                        .with_message("mismatched types")
                        .with_label(expr1.span.clone(), format!("this is of type {}", type1))
                        .with_label(expr2.span.clone(), format!("this is of type {}", type2))
                        .report();
                }

                None
            }

            ExprInner::Logical(lhs, rhs, _) => {
                if self
                    .expression(lhs, Some(&SemanticType::Bool))
                    .is_some_and(|t| t != SemanticType::Bool)
                {
                    self.err_ctx
                        .error(lhs.span.clone())
                        .with_message("expected bool for logical operation")
                        .with_label(lhs.span.clone(), "expected bool")
                        .report();
                }

                if self
                    .expression(rhs, Some(&SemanticType::Bool))
                    .is_some_and(|t| t != SemanticType::Bool)
                {
                    self.err_ctx
                        .error(rhs.span.clone())
                        .with_message("expected bool for logical operation")
                        .with_label(rhs.span.clone(), "expected bool")
                        .report();
                }

                Some(SemanticType::Bool)
            }

            ExprInner::Negate(expr) => {
                let typ = self.expression(expr, None);

                if let Some(typ) = typ {
                    if matches!(typ.sign(), Some(Sign::Unsigned)) {
                        self.err_ctx
                            .error(expr.span.clone())
                            .with_message("cannot negate an unsigned integer")
                            .with_label(expr.span.clone(), "expected signed integer")
                            .report();
                    }

                    if !matches!(typ, SemanticType::I64) {
                        self.err_ctx
                            .error(expr.span.clone())
                            .with_message("cannot negate a non-integer")
                            .with_label(expr.span.clone(), "expected signed integer")
                            .report();
                    }

                    Some(typ)
                } else {
                    Some(SemanticType::I64)
                }
            }

            ExprInner::Not(expr) => {
                if self
                    .expression(expr, None)
                    .is_some_and(|t| t != SemanticType::Bool)
                {
                    self.err_ctx
                        .error(expr.span.clone())
                        .with_message("cannot negate non-boolean value")
                        .with_label(expr.span.clone(), "expected bool")
                        .report();
                }

                Some(SemanticType::Bool)
            }

            ExprInner::Cast(cast_from, cast_to) => {
                if let Some(expr_type) = self.expression(cast_from, None) {
                    if expr_type.can_cast_to(cast_to) {
                        expr.typ = Some(cast_to.clone());
                        return Some(cast_to.clone());
                    }

                    self.err_ctx
                        .error(cast_from.span.clone())
                        .with_message("invalid type cast")
                        .with_label(
                            cast_from.span.clone(),
                            format!("cannot cast from {} to {}", expr_type, cast_to),
                        )
                        .report();
                }

                None
            }

            ExprInner::Index(array, index_expr, size) => {
                let item_type = self.check_index(array, index_expr, &expr.span);

                if let Some(item_type) = item_type.as_ref() {
                    *size = Some(ValSize::from_bytes(self.size_of(item_type)).unwrap());
                }

                item_type
            }

            ExprInner::MemberAccess(parent, member, typename) => {
                let parent_type = self.expression(parent, None);
                match parent_type {
                    Some(
                        SemanticType::UserType(name)
                        | SemanticType::Pointer(deref!(SemanticType::UserType(name))),
                    ) => {
                        if let Some(typ) = self.types.get(&name) {
                            let fieldtype = typ
                                .fields
                                .iter()
                                .find(|(field_name, _, _)| field_name == member);

                            if let Some((_, fieldtype, _)) = fieldtype {
                                *typename = Some(name);
                                Some(fieldtype.clone())
                            } else {
                                self.err_ctx
                                    .error(parent.span.clone())
                                    .with_message("invalid member access")
                                    .with_label(
                                        parent.span.clone(),
                                        format!("{} has no member named {}", name, member),
                                    )
                                    .report();

                                None
                            }
                        } else {
                            self.err_ctx
                                .error(parent.span.clone())
                                .with_message("invalid member access")
                                .with_label(parent.span.clone(), format!("unknown type {}", name))
                                .report();

                            None
                        }
                    }
                    Some(typ) => {
                        self.err_ctx
                            .error(parent.span.clone())
                            .with_message("invalid member access")
                            .with_label(
                                parent.span.clone(),
                                format!("cannot access primitive {}", typ),
                            )
                            .report();

                        None
                    }
                    None => None,
                }
            }

            ExprInner::FnCall(function, call_args) => {
                let call_types: Vec<(SemanticType, Span)> = call_args
                    .iter_mut()
                    .filter_map(|e| self.expression(e, None).map(|t| (t, e.span.clone())))
                    .collect();

                if let Some((fn_decl_span, ret_type, decl_args)) = self.functions.get(function) {
                    if decl_args.len() != call_args.len() {
                        self.err_ctx
                            .error(expr.span.clone())
                            .with_message("invalid argument count")
                            .with_label(
                                expr.span.clone(),
                                format!(
                                    "expected {} arguments, got {}",
                                    decl_args.len(),
                                    call_args.len()
                                ),
                            )
                            .with_label(fn_decl_span.clone(), "function defined here")
                            .report();
                    }

                    for ((call_type, call_span), (decl_span, decl_type)) in
                        call_types.iter().zip(decl_args)
                    {
                        if call_type != decl_type {
                            self.err_ctx
                                .error(call_span.clone())
                                .with_message("incompatible types")
                                .with_label(
                                    call_span.clone(),
                                    format!("this is of type {}", call_type),
                                )
                                .with_label(
                                    decl_span.clone(),
                                    format!("function accepts argument of type {}", decl_type),
                                )
                                .report();
                        }
                    }

                    if !self.fn_call_context.contains(function) {
                        self.fn_call_context.insert(function.to_owned());
                    }

                    Some(ret_type.clone())
                } else {
                    self.err_ctx
                        .error(expr.span.clone())
                        .with_message("invalid function call")
                        .with_label(expr.span.clone(), format!("{} is not a function", function))
                        .report();

                    None
                }
            }

            ExprInner::SizeOf(typ) => {
                self.verify_type(typ, &expr.span);
                Some(SemanticType::U64)
            }
        };

        expr.typ = typ.clone();
        typ
    }

    fn check_index(
        &mut self,
        array: &str,
        index_expr: &mut Expression,
        span: &Span,
    ) -> Option<SemanticType> {
        if let Some(expr_type) = self.expression(index_expr, Some(&SemanticType::U64))
            && expr_type != SemanticType::U64
        {
            self.err_ctx
                .error(span.clone())
                .with_message(format!(
                    "cannot index {} with value of type {}",
                    array, expr_type
                ))
                .with_label(index_expr.span.clone(), "expected u64")
                .report();
        }

        let var_type = self.check_var(array, span)?;

        if !matches!(var_type, SemanticType::Pointer(_)) {
            self.err_ctx
                .error(span.clone())
                .with_message(format!("cannot index variable of type {}", var_type))
                .with_label(index_expr.span.clone(), "expected pointer")
                .report();

            return None;
        }

        let SemanticType::Pointer(item_type) = var_type else {
            unreachable!()
        };

        Some(*item_type)
    }

    fn check_var(&mut self, symbol: &str, span: &Span) -> Option<SemanticType> {
        if let Some(typ) = self.variables.get(symbol).or(self.globals.get(symbol)) {
            return Some(typ.clone());
        }

        self.err_ctx
            .error(span.clone())
            .with_message("undeclared variable")
            .with_label(span.clone(), "this guy doesn't exist")
            .report();

        None
    }

    fn check_ptr(&mut self, symbol: &str, span: &Span) -> Option<SemanticType> {
        if let Some(typ) = self.check_var(symbol, span) {
            match typ {
                SemanticType::Pointer(typ) => return Some(*typ),
                typ => {
                    self.err_ctx
                        .error(span.clone())
                        .with_message("invalid pointer deref")
                        .with_label(span.clone(), format!("cannot derefence type {}", typ))
                        .report();
                }
            }
        }

        None
    }

    fn verify_type(&mut self, typ: &SemanticType, span: &Span) -> bool {
        match typ {
            SemanticType::UserType(name) if !self.types.contains_key(name) => {
                self.err_ctx
                    .error(span.clone())
                    .with_message("unknown type")
                    .with_label(span.clone(), format!("unknown type {}", name))
                    .report();

                false
            }
            SemanticType::Pointer(inner) => self.verify_type(inner, span),
            _ => true,
        }
    }
}

fn combine_span(span: &Span, span_2: &Span) -> Span {
    (span.0.clone(), span.1.start..span_2.1.end)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Sign {
    Signed,
    Unsigned,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SemanticType {
    Unit,
    I8,
    I64,
    U8,
    U64,
    Char,
    Bool,
    Pointer(Box<SemanticType>),
    UserType(String),
}

impl SemanticType {
    pub fn sign(&self) -> Option<Sign> {
        match self {
            SemanticType::Unit => None,
            SemanticType::I8 | SemanticType::I64 => Some(Sign::Signed),
            SemanticType::U8 | SemanticType::U64 => Some(Sign::Unsigned),
            SemanticType::Char => Some(Sign::Unsigned),
            SemanticType::Bool => None,
            SemanticType::Pointer(typ) => typ.sign(),
            SemanticType::UserType { .. } => None,
        }
    }

    pub fn can_cast_to(&self, other: &SemanticType) -> bool {
        use SemanticType::*;

        matches!(
            (self, other),
            (Char, I64)
                | (I64, Char)
                | (Char, U8)
                | (U8, Char)
                | (Pointer(_), I64)
                | (Pointer(_), U64)
                | (I64, Pointer(_))
                | (U64, Pointer(_))
                | (Pointer(_), Pointer(_))
                | (I64, U64)
                | (U64, I64)
        )
    }

    /// If the type is not concretely decided, like an integer constant, the type can be switched to
    /// another compatible type depending on the context.
    pub fn compatible_with(&self, other: &SemanticType) -> bool {
        use SemanticType::*;

        matches!((self, other), (U64, I64) | (I64, U64))
    }
}

impl<S: AsRef<str>> From<S> for SemanticType {
    fn from(string: S) -> Self {
        let string = string.as_ref();

        if let Some(typ) = string.strip_suffix('*') {
            let typ = SemanticType::from(typ);
            return SemanticType::Pointer(Box::new(typ));
        }

        match string {
            "i8" => Self::I8,
            "i64" => Self::I64,
            "u8" => Self::U8,
            "u64" => Self::U64,
            "char" => Self::Char,
            "bool" => Self::Bool,
            name => Self::UserType(name.to_owned()),
        }
    }
}

impl fmt::Display for SemanticType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SemanticType::Unit => write!(f, "()"),
            SemanticType::I8 => write!(f, "i8"),
            SemanticType::I64 => write!(f, "i64"),
            SemanticType::U8 => write!(f, "u8"),
            SemanticType::U64 => write!(f, "u64"),
            SemanticType::Char => write!(f, "char"),
            SemanticType::Bool => write!(f, "bool"),
            SemanticType::Pointer(typ) => write!(f, "&{}", typ),
            SemanticType::UserType(typ) => write!(f, "{}", typ),
        }
    }
}

#[derive(Debug, Clone)]
struct DataType {
    fields: Vec<(String, SemanticType, u64)>,
    size: u64,
}
