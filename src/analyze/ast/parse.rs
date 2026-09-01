use std::{borrow::Cow, ops::Range, path::PathBuf, rc::Rc};

use crate::analyze::{
    Error, ErrorCode, ErrorContext, Span,
    ast::{
        AST, ArithmeticOp, Assignable, CompareOp, ExprInner, Expression, FnDef, Item, LogicalOp,
        ParsedType, Statement,
    },
    lex::{
        Tokens,
        token::{Keyword, Operator, Token},
    },
    semantics::types::{Primitive, Sign},
};

pub struct Parser<'e, 's> {
    tokens: Tokens<'s>,
    source: &'s str,
    ast: AST<'s, (ParsedType<'s>, Span)>,
    err_ctx: &'e mut ErrorContext,
    src_path: Rc<PathBuf>,
}

impl<'e, 's> Parser<'e, 's> {
    pub fn parse(
        tokens: Tokens<'s>,
        source: &'s str,
        src_path: Rc<PathBuf>,
        err_ctx: &'e mut ErrorContext,
    ) -> Result<AST<'s, (ParsedType<'s>, Span)>, ()> {
        let mut parser = Self {
            tokens,
            source,
            ast: AST::<'s>::new(),
            err_ctx,
            src_path,
        };

        while parser.tokens.current().is_some() {
            match parser.parse_item() {
                Ok(Some(item)) => parser.ast.add_item(item),
                Ok(None) => {}
                Err(err) => parser.err_ctx.report(err),
            }
        }

        let ast: AST<'s, (ParsedType<'s>, Span)> = parser.ast;

        Ok(ast)
    }

    fn find_semicolon(&mut self) -> Result<bool, Error> {
        while let Some((token, _)) = self.tokens.take_current() {
            if matches!(token, Token::Semicolon) {
                return Ok(true);
            }
        }

        Ok(false)
    }

    fn parse_item(&mut self) -> Result<Option<Item<'s, (ParsedType<'s>, Span)>>, Error> {
        let (token, range) = self.expect_take_current()?;
        let Token::Keyword(keyword) = token else {
            return Err(self
                .err_ctx
                .unexpected_token(self.span(range), "expected keyword")
                .finish());
        };

        match keyword {
            Keyword::Function => self.parse_function(range.start).map(Some),
            Keyword::Extern => self.parse_extern().map(Some),
            Keyword::Memory => self.parse_memory().map(Some),
            Keyword::Struct => self.parse_struct().map(Some),
            Keyword::Impl => self.parse_impl().map(Some),
            Keyword::Use => {
                let path_start = self.tokens.cur_token_start();
                self.expect_matches(
                    |t| matches!(t, Token::Ident(_)),
                    "expected path to package item",
                )?;

                let import = self.parse_rest_of_path(path_start)?;
                self.expect_semicolon()?;

                self.ast.imports.push((
                    import,
                    self.span(path_start..(self.tokens.last_token_end() - 1)),
                ));

                Ok(None)
            }
            Keyword::Package => {
                let (token, range) = self.expect_take_current()?;
                let Token::Ident(package) = token else {
                    let span = self.span(range);
                    return Err(self
                        .err_ctx
                        .error(span.clone())
                        .with_message("unexpected token")
                        .with_label(span, "expected package identifier")
                        .finish());
                };

                self.expect_semicolon()?;
                self.ast.package = Some(package);

                Ok(None)
            }
            Keyword::Module => {
                let (token, range) = self.expect_take_current()?;
                let Token::Ident(module) = token else {
                    let span = self.span(range);
                    return Err(self
                        .err_ctx
                        .error(span.clone())
                        .with_message("unexpected token")
                        .with_label(span, "expected module identifier")
                        .finish());
                };

                self.expect_semicolon()?;
                self.ast
                    .modules
                    .push((module, self.span(range.start..self.tokens.last_token_end())));

                Ok(None)
            }
            _ => Err(self
                .err_ctx
                .unexpected_token(self.span(range), "expected function or extern import")
                .finish()),
        }
    }

    fn parse_impl(&mut self) -> Result<Item<'s, (ParsedType<'s>, Span)>, Error> {
        let (token, decl_range) = self.expect_take_current()?;
        let decl_span = self.span(decl_range);
        let Token::Ident(struct_name) = token else {
            return Err(self
                .err_ctx
                .error(decl_span.clone())
                .with_message("invalid struct definition")
                .with_label(decl_span, "expected type name")
                .finish());
        };

        self.expect_token(Token::LeftCurlyBracket, "expected opening curly bracket")?;

        let mut functions = Vec::new();
        while let Some((Token::Keyword(Keyword::Function), range)) = self.tokens.current() {
            let decl_start = range.start;
            self.tokens.move_one();
            let func = self.parse_function(decl_start)?;
            let Item::Function(fndef) = func else {
                let span = self.span(decl_start..self.tokens.last_token_end());
                return Err(self
                    .err_ctx
                    .error(span.clone())
                    .with_message("invalid function definition")
                    .with_label(span, "only function implementations allowed")
                    .finish());
            };

            functions.push(fndef);
        }

        self.expect_token(Token::RightCurlyBracket, "expected closing curly bracket")?;

        Ok(Item::Impl {
            struct_name: Cow::Borrowed(struct_name),
            functions,
        })
    }

    fn parse_struct(&mut self) -> Result<Item<'s, (ParsedType<'s>, Span)>, Error> {
        let (token, decl_range) = self.expect_take_current()?;
        let decl_span = self.span(decl_range);
        let Token::Ident(name) = token else {
            return Err(self
                .err_ctx
                .error(decl_span.clone())
                .with_message("invalid struct definition")
                .with_label(decl_span, "expected type name")
                .finish());
        };

        self.expect_token(Token::LeftCurlyBracket, "expected opening curly bracket")?;

        let mut fields = Vec::new();

        while !matches!(self.tokens.current(), Some((Token::RightCurlyBracket, _))) {
            let (token, range) = self.expect_take_current()?;
            let Token::Ident(field_name) = token else {
                let span = self.span(range);
                return Err(self
                    .err_ctx
                    .error(span.clone())
                    .with_message("invalid struct field")
                    .with_label(span, "expected field name")
                    .finish());
            };

            self.expect_token(Token::Colon, "expected colon")?;

            let field_type = self.parse_type()?;

            let field_span = self.span(range.start..self.tokens.last_token_end());

            if matches!(self.tokens.current(), Some((Token::Comma, _))) {
                self.tokens.move_one();
            }

            fields.push((field_name, field_type, field_span));
        }

        self.expect_token(Token::RightCurlyBracket, "expected closing curly bracket")?;

        Ok(Item::Struct {
            name: Cow::Borrowed(name),
            decl_span,
            fields,
        })
    }

    fn parse_memory(&mut self) -> Result<Item<'s, (ParsedType<'s>, Span)>, Error> {
        let (token, range) = self.expect_take_current()?;
        let Token::Ident(name) = token else {
            let span = self.span(range);
            return Err(self
                .err_ctx
                .error(span.clone())
                .with_message("invalid memory statement")
                .with_label(span, "expected identifier")
                .finish());
        };

        self.expect_token(Token::Colon, "expected memory segment type")?;

        let typ = self.parse_type()?;

        self.expect_semicolon()?;

        Ok(Item::MemorySegment { name, typ })
    }

    fn parse_extern(&mut self) -> Result<Item<'s, (ParsedType<'s>, Span)>, Error> {
        let (token, range) = self.expect_take_current()?;
        let Token::Ident(lib) = token else {
            return Err(self
                .err_ctx
                .unexpected_token(self.span(range), "expected library name")
                .finish());
        };

        self.expect_semicolon()?;

        Ok(Item::ExternLib(lib))
    }

    fn parse_function(
        &mut self,
        decl_start: usize,
    ) -> Result<Item<'s, (ParsedType<'s>, Span)>, Error> {
        let (token, range) = self.expect_take_current()?;

        let name = match token {
            Token::Ident(name) => name,
            _ => {
                self.err_ctx
                    .unexpected_token(self.span(range), "expected function name")
                    .report();

                "???"
            }
        };

        self.expect_token(Token::LeftParenthesis, "expected opening parenthesis")?;

        let args = self.parse_decl_args()?;

        self.expect_token(
            Token::RightParenthesis,
            "expected argument or closing parenthesis",
        )?;

        let ret_type = match self.tokens.current() {
            Some((Token::Arrow, _)) => {
                self.tokens.move_one();
                self.parse_type()?
            }
            _ => {
                let cur_tok = self.tokens.cur_token_start();
                (
                    ParsedType::Primitive(Primitive::Unit),
                    self.span(cur_tok..(cur_tok + 1)),
                )
            }
        };

        let decl_end = self.tokens.last_token_end();
        let decl_span = self.span(decl_start..decl_end);

        if matches!(self.tokens.current(), Some((Token::Semicolon, _))) {
            self.tokens.move_one();

            Ok(Item::ForwardDecl {
                name: Cow::Borrowed(name),
                args,
                ret_type,
                decl_span,
            })
        } else {
            let body = self.parse_block()?;

            Ok(Item::Function(FnDef {
                name: Cow::Borrowed(name),
                args,
                body,
                decl_span,
                ret_type,
            }))
        }
    }

    fn parse_decl_args(&mut self) -> Result<Vec<(&'s str, (ParsedType<'s>, Span), Span)>, Error> {
        let mut args = Vec::new();
        while let Some((Token::Ident(name), _)) = self.tokens.current() {
            let name = name.to_owned();

            let rstart = self.tokens.cur_token_start();

            self.tokens.move_one();
            self.expect_matches(
                |t| matches!(t, Token::Colon),
                "expected colon and argument type",
            )?;

            let arg_type = self.parse_type()?;

            let rend = self.tokens.last_token_end();

            args.push((name, arg_type, self.span(rstart..rend)));

            if !matches!(self.tokens.current(), Some((Token::RightParenthesis, _))) {
                self.expect_token(Token::Comma, "expected comma")?;
            }
        }

        Ok(args)
    }

    fn parse_type(&mut self) -> Result<(ParsedType<'s>, Span), Error> {
        let (type_token, range) = self.expect_take_current()?;
        let parsed_type = match type_token {
            Token::Operator(Operator::Star) => ParsedType::Pointer(Box::new(self.parse_type()?.0)),
            Token::Ident(_) => {
                let type_str = self.parse_rest_of_path(range.start)?;
                ParsedType::from(type_str)
            }
            Token::LeftParenthesis
                if matches!(self.tokens.current(), Some((Token::RightParenthesis, _))) =>
            {
                self.tokens.move_one();
                ParsedType::Primitive(Primitive::Unit)
            }
            _ => {
                return Err(self
                    .err_ctx
                    .unexpected_token(self.span(range), "expected type")
                    .finish());
            }
        };

        let span = self.span(range.start..self.tokens.last_token_end());
        Ok((parsed_type, span))
    }

    fn parse_block(&mut self) -> Result<Vec<Statement<'s, (ParsedType<'s>, Span)>>, Error> {
        self.expect_token(Token::LeftCurlyBracket, "expected block")?;

        let mut statements = Vec::new();
        while let Some((token, _)) = self.tokens.current() {
            if matches!(token, Token::RightCurlyBracket) {
                self.tokens.take_current();
                return Ok(statements);
            }

            match self.parse_statement() {
                Ok(stmt) => statements.push(stmt),
                Err(err) => {
                    self.err_ctx.report(err);
                    self.find_semicolon()?;
                }
            }
        }

        Err(self.err_ctx.unexpected_eof(self.span_eof()).finish())
    }

    fn parse_statement(&mut self) -> Result<Statement<'s, (ParsedType<'s>, Span)>, Error> {
        let (token, range) = self.tokens.current().unwrap().clone();

        if let Token::Keyword(keyword) = token {
            self.tokens.take_current();
            self.parse_keyword(keyword, range.clone())
        } else {
            let expr = self.parse_expr()?;

            match self.tokens.take_current() {
                Some((Token::Semicolon, _)) => Ok(Statement::Expr(expr)),
                Some((Token::Assign(op), assign_range)) => {
                    let var = match expr.inner.clone() {
                        ExprInner::Variable(var) => Assignable::Var(var),
                        ExprInner::Deref(var, None) => Assignable::Ptr(var, None),
                        ExprInner::Index(array, index, size) => {
                            Assignable::Index(array, index, size)
                        }
                        ExprInner::MemberAccess(parent, member, _typename) => {
                            Assignable::MemberAccess(parent, member)
                        }
                        _ => {
                            return Err(self
                                .err_ctx
                                .error(expr.span.clone())
                                .with_message("invalid assignment")
                                .with_label(
                                    expr.span,
                                    "only variables, derefs, indexing and member access allowed",
                                )
                                .finish());
                        }
                    };

                    let mut rvalue = self.parse_expr()?;
                    self.expect_semicolon()?;

                    if let Some(op) = op {
                        let Some(arith_op) = op.as_arithmetic() else {
                            let op_span = self.span(assign_range);
                            return Err(self
                                .err_ctx
                                .error(op_span.clone())
                                .with_message("invalid assign operator")
                                .with_label(op_span, "expected +=, -=, *= or /=")
                                .finish());
                        };

                        let span = self.span(expr.span.1.start..rvalue.span.1.end);
                        rvalue = Expression {
                            inner: ExprInner::Arithmetic(
                                Box::new(expr),
                                Box::new(rvalue),
                                arith_op,
                                None,
                            ),
                            typ: None,
                            span,
                        };
                    }

                    Ok(Statement::Assign {
                        var,
                        expr: rvalue,
                        var_span: self.span(range),
                    })
                }
                Some((Token::Declare, _)) => {
                    let ExprInner::Variable(var) = expr.inner else {
                        return Err(self
                            .err_ctx
                            .error(expr.span.clone())
                            .with_message("invalid assignment")
                            .with_label(expr.span, "only variables are allowed in assignments")
                            .finish());
                    };

                    let rvalue = self.parse_expr()?;
                    self.expect_semicolon()?;
                    Ok(Statement::Declare {
                        var,
                        expr: rvalue,
                        var_span: self.span(range),
                    })
                }
                Some((_, range)) => Err(self
                    .err_ctx
                    .unexpected_token(self.span(range), "expected ';', '=' or ':='")
                    .finish()),
                None => Err(self.err_ctx.unexpected_eof(self.span_eof()).finish()),
            }
        }
    }

    fn parse_keyword(
        &mut self,
        keyword: Keyword,
        range: Range<usize>,
    ) -> Result<Statement<'s, (ParsedType<'s>, Span)>, Error> {
        match keyword {
            Keyword::Return => self.parse_return(),
            Keyword::If => self.parse_if(),
            Keyword::While => self.parse_while_loop(),
            _ => Err(self
                .err_ctx
                .unexpected_token(self.span(range), "unexpected keyword")
                .finish()),
        }
    }

    fn parse_return(&mut self) -> Result<Statement<'s, (ParsedType<'s>, Span)>, Error> {
        let expr = self.parse_expr()?;
        self.expect_semicolon()?;

        Ok(Statement::Return(expr))
    }

    fn parse_if(&mut self) -> Result<Statement<'s, (ParsedType<'s>, Span)>, Error> {
        let guard = self.parse_expr()?;
        let body = self.parse_block()?;

        Ok(Statement::If { guard, body })
    }

    fn parse_while_loop(&mut self) -> Result<Statement<'s, (ParsedType<'s>, Span)>, Error> {
        let guard = self.parse_expr()?;
        let body = self.parse_block()?;

        Ok(Statement::WhileLoop { guard, body })
    }

    fn _parse_expr(&mut self) -> Result<Expression<'s, (ParsedType<'s>, Span)>, Error> {
        let lhs = self.parse_addsub()?;
        Ok(lhs)
    }

    fn parse_addsub(&mut self) -> Result<Expression<'s, (ParsedType<'s>, Span)>, Error> {
        let lhs = self.parse_muldiv()?;
        match self.tokens.current() {
            Some((Token::Operator(op @ (Operator::Plus | Operator::Minus)), _)) => {
                let op = op.as_arithmetic().unwrap();
                self.tokens.move_one();

                let rhs = self.parse_addsub()?;
                let span = self.span(lhs.span.1.start..rhs.span.1.end);

                Ok(Expression {
                    inner: ExprInner::Arithmetic(Box::new(lhs), Box::new(rhs), op, None),
                    typ: None,
                    span,
                })
            }
            _ => Ok(lhs),
        }
    }

    fn parse_muldiv(&mut self) -> Result<Expression<'s, (ParsedType<'s>, Span)>, Error> {
        let lhs = self.parse_term()?;
        match self.tokens.current() {
            Some((Token::Operator(op @ (Operator::Star | Operator::Slash)), _)) => {
                let op = op.as_arithmetic().unwrap();
                self.tokens.move_one();

                let rhs = self.parse_muldiv()?;
                let span = self.span(lhs.span.1.start..rhs.span.1.end);

                Ok(Expression {
                    inner: ExprInner::Arithmetic(Box::new(lhs), Box::new(rhs), op, None),
                    typ: None,
                    span,
                })
            }
            _ => Ok(lhs),
        }
    }

    fn parse_term(&mut self) -> Result<Expression<'s, (ParsedType<'s>, Span)>, Error> {
        let (token, range) = self.expect_take_current()?;
        let span = self.span(range.clone());
        let expr = match token {
            Token::Number(num, explicit_type) => Expression {
                inner: ExprInner::Const(num, explicit_type.map(|typ| Primitive::from(typ))),
                typ: None,
                span,
            },
            Token::LeftParenthesis => {
                let expr = self.parse_expr()?;
                self.expect_token(Token::RightParenthesis, "expected closing parenthesis")?;
                expr
            }
            Token::Ident(_) => self.parse_ident_expr(range.start)?,
            _ => {
                return Err(self
                    .err_ctx
                    .unexpected_token(span.clone(), "unexpected term")
                    .with_label(span, "expected value, identifier or parenthesis")
                    .finish());
            }
        };

        Ok(expr)
    }

    fn parse_expr(&mut self) -> Result<Expression<'s, (ParsedType<'s>, Span)>, Error> {
        let mut lhs = self.parse_single_expr()?;

        if let Some((Token::Operator(op), _)) = self.tokens.current() {
            let mut op = *op;

            let left_bind_power = op.precedence();

            self.tokens.take_current();

            let right_side = match self.tokens.peek() {
                Some((Token::Operator(next_op), _)) => Some((next_op.precedence(), *next_op)),
                _ => None,
            };

            let rhs = if let Some((right_bind_power, next_op)) = right_side
                && right_bind_power < left_bind_power
            {
                let rhs = self.parse_single_expr()?;
                lhs = Expression {
                    inner: self.bind_expr(op, lhs, rhs),
                    typ: None,
                    span: self.span(0..1),
                };

                self.tokens.move_one();
                op = next_op;

                self.parse_expr()?
            } else {
                self.parse_expr()?
            };

            let span = self.span((lhs.span.1.start)..(rhs.span.1.end));
            let expr_type = self.bind_expr(op, lhs, rhs);

            return Ok(Expression {
                inner: expr_type,
                typ: None,
                span,
            });
        }

        Ok(lhs)
    }

    fn bind_expr(
        &mut self,
        op: Operator,
        lhs: Expression<'s, (ParsedType<'s>, Span)>,
        rhs: Expression<'s, (ParsedType<'s>, Span)>,
    ) -> ExprInner<'s, (ParsedType<'s>, Span)> {
        match op {
            Operator::Plus => {
                ExprInner::Arithmetic(Box::new(lhs), Box::new(rhs), ArithmeticOp::Add, None)
            }
            Operator::Minus => {
                ExprInner::Arithmetic(Box::new(lhs), Box::new(rhs), ArithmeticOp::Sub, None)
            }
            Operator::Star => {
                ExprInner::Arithmetic(Box::new(lhs), Box::new(rhs), ArithmeticOp::Mul, None)
            }
            Operator::Slash => {
                ExprInner::Arithmetic(Box::new(lhs), Box::new(rhs), ArithmeticOp::Div, None)
            }
            Operator::Modulo => {
                ExprInner::Arithmetic(Box::new(lhs), Box::new(rhs), ArithmeticOp::Mod, None)
            }
            Operator::Equal => {
                ExprInner::Comparison(Box::new(lhs), Box::new(rhs), CompareOp::Equal, None)
            }
            Operator::NotEqual => {
                ExprInner::Comparison(Box::new(lhs), Box::new(rhs), CompareOp::NotEqual, None)
            }
            Operator::Less => {
                ExprInner::Comparison(Box::new(lhs), Box::new(rhs), CompareOp::Less, None)
            }
            Operator::LessOrEqual => {
                ExprInner::Comparison(Box::new(lhs), Box::new(rhs), CompareOp::LessOrEqual, None)
            }
            Operator::Greater => {
                ExprInner::Comparison(Box::new(lhs), Box::new(rhs), CompareOp::Greater, None)
            }
            Operator::GreaterOrEqual => ExprInner::Comparison(
                Box::new(lhs),
                Box::new(rhs),
                CompareOp::GreaterOrEqual,
                None,
            ),
            Operator::And => ExprInner::Logical(Box::new(lhs), Box::new(rhs), LogicalOp::And),
            Operator::Or => ExprInner::Logical(Box::new(lhs), Box::new(rhs), LogicalOp::Or),

            Operator::Not => unreachable!(),

            Operator::Dot => {
                let ExprInner::Variable(member) = rhs.inner else {
                    panic!("rhs: {:?}", rhs);
                };
                ExprInner::MemberAccess(Box::new(lhs), member, None)
            }
        }
    }

    fn parse_single_expr(&mut self) -> Result<Expression<'s, (ParsedType<'s>, Span)>, Error> {
        let token = self.expect_take_current()?;

        // BUG: the largest possible 64-bit unsigned integer doesnt work.
        let expr = match token {
            (Token::Number(num, explicit_type), range) => Expression {
                inner: ExprInner::Const(num, explicit_type),
                typ: None,
                span: self.span(range),
            },
            (Token::Reference, ref_range) => {
                let (token, var_range) = self.expect_take_current()?;
                let Token::Ident(var) = token else {
                    let var_span = self.span(var_range);
                    return Err(self
                        .err_ctx
                        .error(self.span(ref_range))
                        .with_message("invalid pointer")
                        .with_label(var_span, "expected variable")
                        .finish());
                };

                Expression {
                    typ: None,
                    inner: ExprInner::Pointer(var),
                    span: self.span(ref_range.start..var_range.end),
                }
            }
            (Token::Operator(Operator::Star), deref_range) => {
                let (token, var_range) = self.expect_take_current()?;
                let Token::Ident(var) = token else {
                    let var_span = self.span(var_range);
                    return Err(self
                        .err_ctx
                        .error(self.span(deref_range))
                        .with_message("invalid pointer deref")
                        .with_label(var_span, "expected variable")
                        .finish());
                };

                Expression {
                    inner: ExprInner::Deref(var, None),
                    typ: None,
                    span: self.span(deref_range.start..var_range.end),
                }
            }
            (Token::Operator(Operator::Minus), range) => {
                let expr = self.parse_single_expr()?;
                match expr.inner {
                    ExprInner::Const(number, explicit_type) => {
                        let span = self.span(range);

                        if let Some(explicit_type) = explicit_type
                            && matches!(explicit_type.sign(), Some(Sign::Unsigned))
                        {
                            return Err(self
                                .err_ctx
                                .error(span.clone())
                                .with_message("cannot negate an unsigned integer")
                                .with_label(
                                    span,
                                    format!(
                                        "negate operator not applicable to type {}",
                                        explicit_type
                                    ),
                                )
                                .finish());
                        }

                        Expression {
                            inner: ExprInner::Const(-(number as i64) as u64, Some(Primitive::I64)),
                            typ: None,
                            span,
                        }
                    }
                    _ => Expression {
                        inner: ExprInner::Negate(Box::new(expr)),
                        typ: None,
                        span: self.span(range),
                    },
                }
            }
            (Token::Operator(Operator::Not), range) => {
                let expr = self.parse_single_expr()?;

                Expression {
                    inner: ExprInner::Not(Box::new(expr)),
                    typ: None,
                    span: self.span(range),
                }
            }
            (Token::Ident(_), range) => self.parse_ident_expr(range.start)?,
            (Token::Character(c), range) => Expression {
                inner: ExprInner::Character(c),
                typ: None,
                span: self.span(range),
            },
            (Token::String(string), range) => Expression {
                inner: ExprInner::String(string),
                typ: None,
                span: self.span(range),
            },
            (Token::Bool(b), range) => Expression {
                inner: ExprInner::Bool(b),
                typ: None,
                span: self.span(range),
            },
            (Token::LeftParenthesis, _) => {
                let expr = self.parse_expr()?;
                self.expect_token(Token::RightParenthesis, "expected closing parenthesis")?;
                expr
            }
            (Token::Keyword(Keyword::SizeOf), kw_range) => {
                self.expect_token(Token::LeftParenthesis, "expected opening parenthesis")?;
                let typ = self.parse_type()?;
                self.expect_token(Token::RightParenthesis, "expected closing parentehsis")?;

                Expression {
                    inner: ExprInner::SizeOf(typ),
                    typ: None,
                    span: self.span(kw_range.start..self.tokens.last_token_end()),
                }
            }
            (Token::LeftBracket, _) => self.parse_construct()?,
            (_, range) => {
                return Err(self
                    .err_ctx
                    .unexpected_token(self.span(range), "invalid expression")
                    .finish());
            }
        };

        if let Some((Token::Keyword(Keyword::As), _)) = self.tokens.current() {
            self.tokens.move_one();
            let typ = self.parse_type()?;

            let start = expr.span.1.start;
            let end = self.tokens.last_token_end();
            let span = self.span(start..end);

            return Ok(Expression {
                inner: ExprInner::Cast(Box::new(expr), typ),
                typ: None,
                span,
            });
        }

        Ok(expr)
    }

    fn parse_construct(&mut self) -> Result<Expression<'s, (ParsedType<'s>, Span)>, Error> {
        let begin = self.tokens.last_token_end() - 1;

        let typ = self.parse_type()?;

        self.expect_token(Token::RightBracket, "expected closing bracket")?;
        self.expect_token(Token::LeftCurlyBracket, "expected opening brace")?;

        let fields = Vec::new();

        self.expect_token(Token::RightCurlyBracket, "expected closeing brace")?;

        Ok(Expression {
            inner: ExprInner::Construct { typ, fields },
            typ: None,
            span: self.span(begin..self.tokens.last_token_end()),
        })
    }

    fn parse_ident_expr(
        &mut self,
        ident_start: usize,
    ) -> Result<Expression<'s, (ParsedType<'s>, Span)>, Error> {
        let ident = self.parse_rest_of_path(ident_start)?;

        if matches!(self.tokens.current(), Some((Token::LeftParenthesis, _))) {
            self.tokens.move_one();

            let args = self.parse_call_args()?;

            self.expect_token(Token::RightParenthesis, "expected closing parenthesis")?;

            Ok(Expression {
                inner: ExprInner::FnCall(ident, args),
                typ: None,
                span: self.span((ident_start)..(self.tokens.last_token_end())),
            })
        } else if matches!(self.tokens.current(), Some((Token::LeftBracket, _))) {
            self.tokens.move_one();

            let expr = self.parse_expr()?;

            self.expect_token(Token::RightBracket, "expected closing bracket")?;

            Ok(Expression {
                inner: ExprInner::Index(ident, Box::new(expr), None),
                typ: None,
                span: self.span((ident_start)..(self.tokens.last_token_end())),
            })
        } else {
            Ok(Expression {
                inner: ExprInner::Variable(ident),
                typ: None,
                span: self.span((ident_start)..(self.tokens.last_token_end())),
            })
        }
    }

    fn parse_rest_of_path(&mut self, path_start: usize) -> Result<&'s str, Error> {
        while matches!(self.tokens.current(), Some((Token::PathSeparator, _))) {
            self.tokens.move_one();
            self.expect_matches(|t| matches!(t, Token::Ident(_)), "expected identifier")?;
        }

        Ok(&self.source[path_start..self.tokens.last_token_end()])
    }

    fn parse_call_args(&mut self) -> Result<Vec<Expression<'s, (ParsedType<'s>, Span)>>, Error> {
        let mut args = Vec::new();
        let mut first = true;

        while !matches!(self.tokens.current(), Some((Token::RightParenthesis, _))) {
            if !first {
                self.expect_matches(|t| matches!(t, Token::Comma), "expected comma")?;
            }

            let expr = self.parse_expr()?;
            args.push(expr);

            first = false;
        }

        Ok(args)
    }

    fn expect_token(&mut self, token: Token<'s>, message: impl ToString) -> Result<(), Error> {
        self.expect_matches(|t| t == &token, message)
    }

    fn expect_matches<F>(&mut self, matches: F, message: impl ToString) -> Result<(), Error>
    where
        F: FnOnce(&Token<'s>) -> bool,
    {
        let (token, range) = self.expect_take_current()?;
        if !matches(&token) {
            return Err(self
                .err_ctx
                .unexpected_token(self.span(range), message)
                .finish());
        }

        Ok(())
    }

    fn expect_semicolon(&mut self) -> Result<(), Error> {
        let current = self.tokens.take_current();
        if !matches!(current, Some((Token::Semicolon, _))) {
            let pos = current
                .map(|t| t.1.start)
                .unwrap_or(self.tokens.cur_token_start());

            let insert_span = self.span((pos - 1)..pos);
            self.err_ctx
                .error(self.span(pos..(pos + 1)))
                .with_code(ErrorCode::MissingSemicolon)
                .with_message("expected semicolon")
                .with_label(insert_span, "insert the semicolon dummy")
                .report();
        }

        Ok(())
    }

    fn expect_take_current(&mut self) -> Result<(Token<'s>, Range<usize>), Error> {
        let token = self.tokens.take_current();
        match token {
            Some(token) => Ok(token),
            None => Err(self.err_ctx.unexpected_eof(self.span_eof()).finish()),
        }
    }

    fn span(&self, range: Range<usize>) -> (Rc<PathBuf>, Range<usize>) {
        (self.src_path.clone(), range)
    }

    fn span_eof(&self) -> (Rc<PathBuf>, Range<usize>) {
        let end = self.tokens.cur_token_start();
        self.span((end - 1)..end)
    }
}

// #[cfg(test)]
// mod tests {
//     use super::*;
//     use crate::analyze::lex::Lexer;
//
//     fn mod_main() -> Rc<PathBuf> {
//         Rc::new(PathBuf::from("main"))
//     }
//
//     fn get_parser(src: &str) -> Parser {
//         let lexer = Lexer::lex(src, mod_main(), src).unwrap();
//         Parser::new(mod_main(), lexer)
//     }
//
//     #[test]
//     fn expr_addsub() {
//         let ast = get_parser("2 + 3 - 4").parse_expr().unwrap();
//         assert!(matches!(
//             ast,
//             Expression {
//                 inner: ExprInner::Arithmetic(
//                     deref!(Expression {
//                         inner: ExprInner::Const(2, None),
//                         ..
//                     }),
//                     deref!(Expression {
//                         inner: ExprInner::Arithmetic(
//                             deref!(Expression {
//                                 inner: ExprInner::Const(3, None),
//                                 ..
//                             }),
//                             deref!(Expression {
//                                 inner: ExprInner::Const(4, None),
//                                 ..
//                             }),
//                             ArithmeticOp::Sub,
//                             _
//                         ),
//                         ..
//                     }),
//                     ArithmeticOp::Add,
//                     _
//                 ),
//                 ..
//             }
//         ));
//     }
//
//     #[test]
//     fn expr_muldiv() {
//         let ast = get_parser("2 * 3 / 4").parse_expr().unwrap();
//         assert!(matches!(
//             ast,
//             Expression {
//                 inner: ExprInner::Arithmetic(
//                     deref!(Expression {
//                         inner: ExprInner::Const(2, None),
//                         ..
//                     }),
//                     deref!(Expression {
//                         inner: ExprInner::Arithmetic(
//                             deref!(Expression {
//                                 inner: ExprInner::Const(3, None),
//                                 ..
//                             }),
//                             deref!(Expression {
//                                 inner: ExprInner::Const(4, None),
//                                 ..
//                             }),
//                             ArithmeticOp::Div,
//                             _
//                         ),
//                         ..
//                     }),
//                     ArithmeticOp::Mul,
//                     _
//                 ),
//                 ..
//             }
//         ));
//     }
//
//     #[test]
//     fn expr_precedence_add_mul() {
//         let ast = get_parser("2 + 3 * 4").parse_expr().unwrap();
//         eprintln!("{:#?}", ast);
//         assert!(matches!(
//             ast,
//             Expression {
//                 inner: ExprInner::Arithmetic(
//                     deref!(Expression {
//                         inner: ExprInner::Const(2, None),
//                         ..
//                     }),
//                     deref!(Expression {
//                         inner: ExprInner::Arithmetic(
//                             deref!(Expression {
//                                 inner: ExprInner::Const(3, None),
//                                 ..
//                             }),
//                             deref!(Expression {
//                                 inner: ExprInner::Const(4, None),
//                                 ..
//                             }),
//                             ArithmeticOp::Mul,
//                             _
//                         ),
//                         ..
//                     }),
//                     ArithmeticOp::Add,
//                     _
//                 ),
//                 ..
//             }
//         ));
//     }
//
//     #[test]
//     fn expr_precedence_mul_add() {
//         let ast = get_parser("2 * 3 + 4").parse_expr().unwrap();
//         eprintln!("{:#?}", ast);
//         assert!(matches!(
//             ast,
//             Expression {
//                 inner: ExprInner::Arithmetic(
//                     deref!(Expression {
//                         inner: ExprInner::Arithmetic(
//                             deref!(Expression {
//                                 inner: ExprInner::Const(2, None),
//                                 ..
//                             }),
//                             deref!(Expression {
//                                 inner: ExprInner::Const(3, None),
//                                 ..
//                             }),
//                             ArithmeticOp::Mul,
//                             _
//                         ),
//                         ..
//                     }),
//                     deref!(Expression {
//                         inner: ExprInner::Const(4, None),
//                         ..
//                     }),
//                     ArithmeticOp::Add,
//                     _
//                 ),
//                 ..
//             }
//         ));
//     }
//
//     #[test]
//     fn expr_parenthesis() {
//         let ast = get_parser("2 * (3 + 4)").parse_expr().unwrap();
//         eprintln!("{:#?}", ast);
//         assert!(matches!(
//             ast,
//             Expression {
//                 inner: ExprInner::Arithmetic(
//                     deref!(Expression {
//                         inner: ExprInner::Const(2, None),
//                         ..
//                     }),
//                     deref!(Expression {
//                         inner: ExprInner::Arithmetic(
//                             deref!(Expression {
//                                 inner: ExprInner::Const(3, None),
//                                 ..
//                             }),
//                             deref!(Expression {
//                                 inner: ExprInner::Const(4, None),
//                                 ..
//                             }),
//                             ArithmeticOp::Add,
//                             _
//                         ),
//                         ..
//                     }),
//                     ArithmeticOp::Mul,
//                     _
//                 ),
//                 ..
//             }
//         ));
//     }
//
//     #[test]
//     fn expr_combo() {
//         let ast = get_parser("1 + 2 * 3 - (4 + 5) / 6").parse_expr().unwrap();
//         eprintln!("{:#?}", ast);
//         assert!(matches!(
//             ast,
//             Expression {
//                 inner: ExprInner::Arithmetic(
//                     deref!(Expression {
//                         inner: ExprInner::Const(1, None),
//                         ..
//                     }),
//                     deref!(Expression {
//                         inner: ExprInner::Arithmetic(
//                             deref!(Expression {
//                                 inner: ExprInner::Arithmetic(
//                                     deref!(Expression {
//                                         inner: ExprInner::Const(2, None),
//                                         ..
//                                     }),
//                                     deref!(Expression {
//                                         inner: ExprInner::Const(3, None),
//                                         ..
//                                     }),
//                                     ArithmeticOp::Mul,
//                                     _
//                                 ),
//                                 ..
//                             }),
//                             deref!(Expression {
//                                 inner: ExprInner::Arithmetic(
//                                     deref!(Expression {
//                                         inner: ExprInner::Arithmetic(
//                                             deref!(Expression {
//                                                 inner: ExprInner::Const(4, None),
//                                                 ..
//                                             }),
//                                             deref!(Expression {
//                                                 inner: ExprInner::Const(5, None),
//                                                 ..
//                                             }),
//                                             ArithmeticOp::Add,
//                                             _
//                                         ),
//                                         ..
//                                     }),
//                                     deref!(Expression {
//                                         inner: ExprInner::Const(6, None),
//                                         ..
//                                     }),
//                                     ArithmeticOp::Div,
//                                     _
//                                 ),
//                                 ..
//                             }),
//                             ArithmeticOp::Sub,
//                             _
//                         ),
//                         ..
//                     }),
//                     ArithmeticOp::Add,
//                     _
//                 ),
//                 ..
//             }
//         ));
//     }
//
//     #[test]
//     fn expr_ident() {
//         let ast = get_parser("2 + pi * 4").parse_expr().unwrap();
//         eprintln!("{:#?}", ast);
//         assert!(matches!(
//             ast,
//             Expression {
//                 inner: ExprInner::Arithmetic(
//                     deref!(Expression {
//                         inner: ExprInner::Const(2, None),
//                         ..
//                     }),
//                     deref!(Expression {
//                         inner: ExprInner::Arithmetic(
//                             deref!(Expression {
//                                 inner: ExprInner::Variable("pi"),
//                                 ..
//                             }),
//                             deref!(Expression {
//                                 inner: ExprInner::Const(4, None),
//                                 ..
//                             }),
//                             ArithmeticOp::Mul,
//                             _
//                         ),
//                         ..
//                     }),
//                     ArithmeticOp::Add,
//                     _
//                 ),
//                 ..
//             }
//         ));
//     }
// }
