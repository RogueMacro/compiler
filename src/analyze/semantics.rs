use std::collections::{HashMap, HashSet};

use crate::{
    analyze::{
        ErrorContext, ErrorVec, Span,
        ast::{AST, Assignable, ExprInner, Expression, FnDef, Item, Statement},
        semantics::types::{ParsedType, Sign, TypeId, TypeKind, TypeMap},
    },
    ir::ValSize,
};

use itertools::Itertools;

pub mod types;

pub struct ValidAST<'s>(pub AST<'s, TypeId>);

const DEFAULT_CAST_PAIRS: &[(&str, &str)] = &[("u64", "i64"), ("i64", "i8"), ("i8", "char")];

pub fn analyze<'s>(
    mut ast: AST<'s, TypeId>,
    types: TypeMap,
    main_fn: impl Into<String>,
) -> Result<(ValidAST<'s>, Analyzer<'s>), ErrorVec> {
    let mut analyzer = Analyzer::new(main_fn, types);
    analyzer.analyze(&mut ast)?;

    Ok((ValidAST(ast), analyzer))
}

pub struct Analyzer<'s> {
    err_ctx: ErrorContext,

    main_fn: String,
    variables: HashMap<String, TypeId>,
    globals: HashMap<&'s str, TypeId>,
    functions: HashMap<String, (Span, TypeId, Vec<(Span, TypeId)>)>,
    function_calls: HashMap<String, HashSet<String>>,
    fn_call_context: HashSet<String>,
    // struct_defs: HashMap<&'s str, Vec<(&'s str, TypeId, Span)>>,
    pub types: TypeMap,

    type_casts: HashSet<(TypeId, TypeId)>,
}

impl<'s> Analyzer<'s> {
    pub fn new(main_fn: impl Into<String>, types: TypeMap) -> Self {
        let type_casts = DEFAULT_CAST_PAIRS
            .iter()
            .map(|(from, to)| {
                (
                    TypeId::from_parsed(&ParsedType::from(*from)),
                    TypeId::from_parsed(&ParsedType::from(*to)),
                )
            })
            .collect();

        Self {
            err_ctx: ErrorContext::new(),

            main_fn: main_fn.into(),
            variables: HashMap::new(),
            globals: HashMap::new(),
            functions: HashMap::new(),
            function_calls: HashMap::new(),
            fn_call_context: HashSet::new(),
            // struct_defs: HashMap::new(),
            types,
            type_casts,
        }
    }

    pub fn analyze(&mut self, ast: &mut AST<'s, TypeId>) -> Result<(), ErrorVec> {
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
                        name.to_string(),
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
                    self.globals.insert(name, *typ);
                }
                Item::Struct {
                    name,
                    decl_span,
                    fields,
                } => {}
            }
        }

        for item in &mut ast.items {
            self.item(item);
        }

        let mut used_functions: HashSet<String> = HashSet::new();
        used_functions.insert(self.main_fn.clone());
        used_functions.insert("std::alloc".to_owned());

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
            Item::Function(FnDef { name, .. }) => used_functions.contains(name.as_ref()),
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

    fn item(&mut self, item: &mut Item<'s, TypeId>) {
        self.variables.clear();

        match item {
            Item::Function(FnDef {
                name,
                args,
                body,
                decl_span,
                ret_type,
            }) => {
                for (arg, typ, _) in args {
                    self.variables.insert(arg.to_owned(), *typ);
                }

                let has_return = self.body(body, *ret_type, decl_span);

                if !has_return && (name == &self.main_fn || *ret_type != TypeId::unit()) {
                    self.err_ctx
                        .error(decl_span.clone())
                        .with_message("no return statement found in function main")
                        .with_label(decl_span.clone(), "main must return a value")
                        .report();
                }

                let calls = std::mem::take(&mut self.fn_call_context);
                self.function_calls.insert(name.to_string(), calls);
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
                    } = fndef;

                    let name = format!("{}::{}", struct_name, name);

                    for (arg, typ, _) in args {
                        self.variables.insert(arg.to_owned(), typ.clone());
                    }

                    let has_return = self.body(body, *ret_type, decl_span);

                    if !has_return && (name == self.main_fn || *ret_type != TypeId::unit()) {
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
            } => {}
        }
    }

    /// Returns whether this statement contains a return statement
    fn body(
        &mut self,
        body: &mut [Statement<'s, TypeId>],
        fn_ret_type: TypeId,
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
        stmt: &mut Statement<'s, TypeId>,
        fn_ret_type: TypeId,
        fn_decl_span: &Span,
    ) -> bool {
        match stmt {
            Statement::Declare {
                var,
                expr,
                var_span,
                explicit_type,
            } => {
                let expr_type = self.expression(expr, None);

                if let Some(expr_type) = expr_type
                    && let Some(explicit_type) = explicit_type
                    && expr_type != *explicit_type
                {
                    let expr_msg = format!(
                        "expected type {}, found type {}",
                        self.types.display(*explicit_type),
                        self.types.display(expr_type)
                    );
                    self.err_ctx
                        .error(var_span.clone())
                        .with_message("unexpected type")
                        .with_label(expr.span.clone(), expr_msg)
                        .with_note(var_span.clone(), "variable defined here")
                        .report();
                }

                if self
                    .variables
                    .insert(var.to_owned(), expr_type.unwrap_or(TypeId::unit()))
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
                            *size = Some(ValSize::from_bytes(self.types.size_of(*typ)).unwrap());
                        }

                        typ
                    }
                    Assignable::Index {
                        data,
                        index,
                        val_size,
                    } => {
                        let item_type = self.check_index(data, index, var_span);

                        if let Some(item_type) = item_type {
                            *val_size =
                                Some(ValSize::from_bytes(self.types.size_of(item_type)).unwrap());
                        }

                        None
                    }
                    Assignable::MemberAccess(parent, member) => {
                        match self.expression(parent, None) {
                            Some(typeid) => {
                                let typeinfo = self.types.get(typeid);

                                if let TypeKind::Pointer(value_typeid) = typeinfo.kind {
                                    let value_typeinfo = self.types.get(value_typeid);
                                    if let TypeKind::Struct { qualifier, fields } =
                                        &value_typeinfo.kind
                                    {
                                        let field_type = fields.iter().find_map(|(n, t, _, _)| {
                                            if n == member { Some(t.clone()) } else { None }
                                        });

                                        if field_type.is_none() {
                                            self.err_ctx
                                                .error(var_span.clone())
                                                .with_message("invalid member access")
                                                .with_label(
                                                    var_span.clone(),
                                                    format!(
                                                        "type {} has no member {}",
                                                        qualifier, member
                                                    ),
                                                )
                                                .report();
                                        }

                                        field_type
                                    } else {
                                        self.err_ctx
                                            .error(var_span.clone())
                                            .with_message("can only access struct types")
                                            .with_label(
                                                var_span.clone(),
                                                format!(
                                                    "expected struct, found type {}",
                                                    self.types.display(typeid)
                                                ),
                                            )
                                            .report();

                                        None
                                    }
                                } else {
                                    let msg =
                                        format!("this is of type {}", self.types.display(typeid));
                                    self.err_ctx
                                        .error(var_span.clone())
                                        .with_message("expected pointer")
                                        .with_label(var_span.clone(), msg)
                                        .report();

                                    None
                                }
                            }
                            None => None,
                        }
                    }
                };

                if let Some(assign_type) = assign_type
                    && let Some(decl_type) = decl_type
                    && assign_type != decl_type
                {
                    let decl_msg = format!("this is of type {}", self.types.display(decl_type));
                    let assign_msg = format!("this is of type {}", self.types.display(assign_type));
                    self.err_ctx
                        .error(combine_span(var_span, &expr.span))
                        .with_message("mismatched types")
                        .with_note(var_span.clone(), decl_msg)
                        .with_label(expr.span.clone(), assign_msg)
                        .report();
                }
            }
            Statement::If { guard, body } | Statement::WhileLoop { guard, body } => {
                if let Some(typeid) = self.expression(guard, Some(TypeId::bool()))
                    && typeid != TypeId::bool()
                {
                    let message =
                        format!("expected type 'bool', got '{}'", self.types.display(typeid));
                    self.err_ctx
                        .error(guard.span.clone())
                        .with_message("unexpected type")
                        .with_label(guard.span.clone(), message)
                        .report();
                }

                return self.body(body, fn_ret_type, fn_decl_span);
            }
            Statement::Expr(expr) => {
                self.expression(expr, None);
            }
            Statement::Return(expr) => {
                if let Some(typeid) = self.expression(expr, Some(fn_ret_type))
                    && typeid != fn_ret_type
                {
                    let actual_ret = format!("this is of type {}", self.types.display(typeid));
                    let expected_ret =
                        format!("function returns {}", self.types.display(fn_ret_type));
                    self.err_ctx
                        .error(expr.span.clone())
                        .with_message("incompatible types")
                        .with_label(expr.span.clone(), actual_ret)
                        .with_label(fn_decl_span.clone(), expected_ret)
                        .report();
                }

                return true;
            }
        }

        false
    }

    fn expression(
        &mut self,
        expr: &mut Expression<'s, TypeId>,
        hint: Option<TypeId>,
    ) -> Option<TypeId> {
        let typeid = match &mut expr.inner {
            ExprInner::Const(_, explicit_type) => Some(
                explicit_type
                    .map(TypeId::from_primitive)
                    .or_else(|| hint.filter(|hint| hint.compatible_with(TypeId::i64())))
                    .unwrap_or(TypeId::i64()), // explicit_type
                                               //     .clone()
                                               //     .or_else(|| {
                                               //         hint.filter(|hint| hint.compatible_with(TypeId::i64()))
                                               //             .cloned()
                                               //     })
                                               //     .unwrap_or(TypeId::i64()),
            ),
            ExprInner::Character(_) => Some(TypeId::char()),
            ExprInner::String(_) => Some(TypeId::char_ptr()),
            ExprInner::Bool(_) => Some(TypeId::bool()),

            ExprInner::Variable(var) => self.check_var(var, &expr.span),
            ExprInner::Pointer(var) => self
                .check_var(var, &expr.span)
                .map(|typeid| self.types.ptr_type_to(typeid)),
            ExprInner::Deref(var, typeid) => {
                *typeid = self.check_ptr(var, &expr.span);
                *typeid
            }

            ExprInner::Arithmetic(expr1, expr2, _op, expr_sign) => {
                if let Some(type1) = self.expression(expr1, hint)
                    && let Some(type2) = self.expression(expr2, Some(type1))
                {
                    if type1 == type2 {
                        if let Some(type_sign) = self.types.get(type1).sign() {
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

                    let type1_msg = format!("this is of type {}", self.types.display(type1));
                    let type2_msg = format!("this is of type {}", self.types.display(type2));
                    self.err_ctx
                        .error(combine_span(&expr1.span, &expr2.span))
                        .with_message("mismatched types")
                        .with_label(expr1.span.clone(), type1_msg)
                        .with_label(expr2.span.clone(), type2_msg)
                        .report();
                }

                None
            }

            ExprInner::Comparison(expr1, expr2, _op, expr_sign) => {
                if let Some(type1) = self.expression(expr1, None)
                    && let Some(type2) = self.expression(expr2, None)
                {
                    if type1 == type2 {
                        let sign1 = self.types.get(type1).sign();
                        let sign2 = self.types.get(type2).sign();
                        if sign1 == sign2 {
                            *expr_sign = sign1;
                            expr.typ = Some(type1.clone());
                            return Some(TypeId::bool());
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

                    let type1_msg = format!("this is of type {}", self.types.display(type1));
                    let type2_msg = format!("this is of type {}", self.types.display(type2));
                    self.err_ctx
                        .error(combine_span(&expr1.span, &expr2.span))
                        .with_message("mismatched types")
                        .with_label(expr1.span.clone(), type1_msg)
                        .with_label(expr2.span.clone(), type2_msg)
                        .report();
                }

                None
            }

            ExprInner::Logical(lhs, rhs, _) => {
                if self
                    .expression(lhs, Some(TypeId::bool()))
                    .is_some_and(|t| t != TypeId::bool())
                {
                    self.err_ctx
                        .error(lhs.span.clone())
                        .with_message("expected bool for logical operation")
                        .with_label(lhs.span.clone(), "expected bool")
                        .report();
                }

                if self
                    .expression(rhs, Some(TypeId::bool()))
                    .is_some_and(|t| t != TypeId::bool())
                {
                    self.err_ctx
                        .error(rhs.span.clone())
                        .with_message("expected bool for logical operation")
                        .with_label(rhs.span.clone(), "expected bool")
                        .report();
                }

                Some(TypeId::bool())
            }

            ExprInner::Negate(expr) => {
                let typ = self.expression(expr, None);

                if let Some(typeid) = typ {
                    if matches!(self.types.get(typeid).sign(), Some(Sign::Unsigned)) {
                        self.err_ctx
                            .error(expr.span.clone())
                            .with_message("cannot negate an unsigned integer")
                            .with_label(expr.span.clone(), "expected signed integer")
                            .report();
                    }

                    if typeid != TypeId::i64() {
                        // TODO: other integer types
                        self.err_ctx
                            .error(expr.span.clone())
                            .with_message("cannot negate a non-integer")
                            .with_label(expr.span.clone(), "expected signed integer")
                            .report();
                    }

                    Some(typeid)
                } else {
                    Some(TypeId::bool())
                }
            }

            ExprInner::Not(expr) => {
                if self
                    .expression(expr, None)
                    .is_some_and(|t| t != TypeId::bool())
                {
                    self.err_ctx
                        .error(expr.span.clone())
                        .with_message("cannot negate non-boolean value")
                        .with_label(expr.span.clone(), "expected bool")
                        .report();
                }

                Some(TypeId::bool())
            }

            ExprInner::Cast(cast_from, cast_to) => {
                if let Some(expr_type) = self.expression(cast_from, None) {
                    let expr_type_is_ptr = self.types.get(expr_type).is_ptr();
                    let cast_to_is_ptr = self.types.get(*cast_to).is_ptr();

                    if self.type_casts.contains(&(expr_type, *cast_to))
                        || self.type_casts.contains(&(*cast_to, expr_type))
                        || (expr_type_is_ptr && cast_to_is_ptr)
                        || (expr_type_is_ptr
                            && (*cast_to == TypeId::i64() || *cast_to == TypeId::u64()))
                        || ((expr_type == TypeId::i64() || expr_type == TypeId::u64())
                            && cast_to_is_ptr)
                    {
                        expr.typ = Some(*cast_to);
                    } else {
                        let msg = format!(
                            "cannot cast from {} to {}",
                            self.types.display(expr_type),
                            self.types.display(*cast_to)
                        );
                        self.err_ctx
                            .error(cast_from.span.clone())
                            .with_message("invalid type cast")
                            .with_label(cast_from.span.clone(), msg)
                            .report();
                    }
                }

                Some(*cast_to)
            }

            ExprInner::Index {
                data,
                index,
                val_size,
            } => {
                let item_type = self.check_index(data, index, &expr.span);

                if let Some(item_type) = item_type.as_ref() {
                    *val_size = Some(ValSize::from_bytes(self.types.size_of(*item_type)).unwrap());
                }

                item_type
            }

            ExprInner::MemberAccess(parent, member, typeid) => {
                let parent_type =
                    self.expression(parent, None)
                        .map(|t| match &self.types.get(t).kind {
                            TypeKind::Pointer(typeid) => (*typeid, &self.types.get(*typeid).kind),
                            other => (t, other),
                        });

                match parent_type {
                    Some((parent_typeid, TypeKind::Struct { qualifier, fields })) => {
                        let fieldtype = fields
                            .iter()
                            .find(|(field_name, _, _, _)| field_name == member);

                        if let Some((_, fieldtype, _, _)) = fieldtype {
                            *typeid = Some(parent_typeid);
                            Some(*fieldtype)
                        } else {
                            self.err_ctx
                                .error(parent.span.clone())
                                .with_message("invalid member access")
                                .with_label(
                                    parent.span.clone(),
                                    format!("{} has no member named {}", qualifier, member),
                                )
                                .report();

                            None
                        }
                    }
                    Some((typeid, _)) => {
                        let msg = format!("cannot access type {}", self.types.display(typeid));
                        self.err_ctx
                            .error(parent.span.clone())
                            .with_message("invalid member access")
                            .with_label(parent.span.clone(), msg)
                            .report();

                        None
                    }
                    None => None,
                }
            }

            ExprInner::FnCall(function, call_args) => {
                let call_types: Vec<(TypeId, Span)> = call_args
                    .iter_mut()
                    .filter_map(|e| self.expression(e, None).map(|t| (t, e.span.clone())))
                    .collect();

                if let Some((fn_decl_span, ret_type, decl_args)) =
                    self.functions.get(function.as_ref())
                {
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
                            .with_note(fn_decl_span.clone(), "function defined here")
                            .report();
                    }

                    for ((call_type, call_span), (decl_span, decl_type)) in
                        call_types.iter().zip(decl_args)
                    {
                        if call_type != decl_type {
                            let call_msg =
                                format!("this is of type {}", self.types.display(*call_type));
                            let decl_msg = format!(
                                "function accepts argument of type {}",
                                self.types.display(*decl_type)
                            );
                            self.err_ctx
                                .error(call_span.clone())
                                .with_message("incompatible types")
                                .with_label(call_span.clone(), call_msg)
                                .with_note(decl_span.clone(), decl_msg)
                                .report();
                        }
                    }

                    if !self.fn_call_context.contains(function.as_ref()) {
                        self.fn_call_context.insert(function.clone().into_owned());
                    }

                    Some(*ret_type)
                } else {
                    self.err_ctx
                        .error(expr.span.clone())
                        .with_message("invalid function call")
                        .with_label(expr.span.clone(), format!("{} is not a function", function))
                        .report();

                    None
                }
            }

            ExprInner::Construct { typ, fields } => {
                let typeinfo = self.types.get(*typ);
                if let TypeKind::Struct {
                    fields: type_fields,
                    ..
                } = &typeinfo.kind
                {
                    let type_fields = type_fields.clone();
                    let mut missing_fields = Vec::new();

                    for (type_field_name, type_field_type, _, field_def_span) in type_fields.iter()
                    {
                        if let Some((_, init_expr)) =
                            fields.iter_mut().find(|(n, _)| *n == type_field_name)
                        {
                            if let Some(expr_typeid) =
                                self.expression(init_expr, Some(*type_field_type))
                                && expr_typeid != *type_field_type
                            {
                                let msg = format!(
                                    "expected type {}",
                                    self.types.display(*type_field_type)
                                );
                                self.err_ctx
                                    .error(init_expr.span.clone())
                                    .with_message("invalid expression type")
                                    .with_label(init_expr.span.clone(), msg)
                                    .with_note(field_def_span.clone(), "field type defined here")
                                    .report();
                            }
                        } else {
                            missing_fields.push(type_field_name);
                        }
                    }

                    for (name, init_expr) in fields.iter() {
                        if !type_fields.iter().any(|(tn, _, _, _)| tn == *name) {
                            let msg = format!(
                                "type {} does not have a field named {}",
                                self.types.display(*typ),
                                name
                            );
                            self.err_ctx
                                .error(init_expr.span.clone())
                                .with_message("unknown field")
                                .with_label(init_expr.span.clone(), msg)
                                .report();
                        }
                    }

                    if !missing_fields.is_empty() {
                        self.err_ctx
                            .error(expr.span.clone())
                            .with_message(format!(
                                "missing field{} {}",
                                if missing_fields.len() > 1 { "s" } else { "" },
                                missing_fields.iter().join(", ")
                            ))
                            .with_label(expr.span.clone(), "add missing fields")
                            .report();
                    }
                } else {
                    self.err_ctx
                        .error(expr.span.clone())
                        .with_message("invalid construct type")
                        .with_label(expr.span.clone(), "expected struct")
                        .report();
                }

                Some(self.types.ptr_type_to(*typ))
            }

            ExprInner::SizeOf(typ) => Some(TypeId::u64()),
        };

        expr.typ = typeid;
        typeid
    }

    fn check_index(
        &mut self,
        array: &mut Expression<'s, TypeId>,
        index_expr: &mut Expression<'s, TypeId>,
        span: &Span,
    ) -> Option<TypeId> {
        let var_type = self.expression(array, None)?;

        if let Some(expr_type) = self.expression(index_expr, Some(TypeId::u64()))
            && expr_type != TypeId::u64()
        {
            let message = format!(
                "cannot index type {} with value of type {}",
                self.types.display(var_type),
                self.types.display(expr_type)
            );
            self.err_ctx
                .error(span.clone())
                .with_message(message)
                .with_label(index_expr.span.clone(), "expected u64")
                .report();
        }

        let var_type_info = self.types.get(var_type);
        if let TypeKind::Pointer(deref_type) = var_type_info.kind {
            return Some(deref_type);
        }

        let msg = format!(
            "cannot index variable of type {}",
            self.types.display(var_type)
        );
        self.err_ctx
            .error(span.clone())
            .with_message(msg)
            .with_label(index_expr.span.clone(), "expected pointer")
            .report();

        None
    }

    fn check_var(&mut self, symbol: &str, span: &Span) -> Option<TypeId> {
        if let Some(typ) = self.variables.get(symbol).or(self.globals.get(symbol)) {
            return Some(*typ);
        }

        self.err_ctx
            .error(span.clone())
            .with_message("undeclared variable")
            .with_label(span.clone(), "this guy doesn't exist")
            .report();

        None
    }

    fn check_ptr(&mut self, symbol: &str, span: &Span) -> Option<TypeId> {
        if let Some(typeid) = self.check_var(symbol, span) {
            let typeinfo = self.types.get(typeid);
            match typeinfo.deref_type() {
                Some(typ) => return Some(typ),
                typ => {
                    let msg = format!("cannot derefence type {}", self.types.display(typeid));
                    self.err_ctx
                        .error(span.clone())
                        .with_message("invalid pointer deref")
                        .with_label(span.clone(), msg)
                        .report();
                }
            }
        }

        None
    }
}

fn combine_span(span: &Span, span_2: &Span) -> Span {
    (span.0.clone(), span.1.start..span_2.1.end)
}
