use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
};

use crate::{
    analyze::{
        ast::{
            ArithmeticOp, Assignable, ExprInner, Expression, FnDef, Item as AstItem, LogicalOp,
            Statement,
        },
        semantics::{
            Analyzer, ValidAST,
            types::{ParsedType, Sign, TypeId, TypeKind, TypeMap},
        },
    },
    ir::{BasicBlock, Condition, IR, Item, Label, Op, SourceVal, Terminator, ValSize, VirtualReg},
};

impl<'s> IR<'s> {
    pub fn generate(ast: ValidAST<'s>, analyzer: &Analyzer<'s>) -> IR<'s> {
        let ast = ast.0;

        let mut ir = IR::default();

        for item in ast.items {
            match item {
                AstItem::Function(FnDef {
                    name, body, args, ..
                }) => {
                    let initial_args: Vec<_> = args
                        .into_iter()
                        .enumerate()
                        .map(|(i, (name, _, _))| (name, VirtualReg(i as u32)))
                        .collect();

                    let vreg_args = initial_args.iter().map(|(_, vreg)| *vreg).collect();

                    let (body, stack, stack_size, size_map) =
                        BlockBuilder::new(analyzer, &mut ir, initial_args).build(body);

                    ir.items.push(Item::Function {
                        name,
                        args: vreg_args,
                        stack,
                        stack_size,
                        size_map,
                        body,
                    });
                }

                AstItem::Impl {
                    struct_name,
                    functions,
                } => {
                    for fndef in functions {
                        let FnDef {
                            name, body, args, ..
                        } = fndef;

                        let name = format!("{}::{}", struct_name, name);

                        let initial_args: Vec<_> = args
                            .into_iter()
                            .enumerate()
                            .map(|(i, (name, _, _))| (name, VirtualReg(i as u32)))
                            .collect();

                        let vreg_args = initial_args.iter().map(|(_, vreg)| *vreg).collect();

                        let (body, stack, stack_size, size_map) =
                            BlockBuilder::new(analyzer, &mut ir, initial_args).build(body);

                        ir.items.push(Item::Function {
                            name: Cow::Owned(name),
                            args: vreg_args,
                            stack,
                            stack_size,
                            size_map,
                            body,
                        });
                    }
                }

                AstItem::MemorySegment { name, typ: typeid } => {
                    let type_size = analyzer.types.size_of(typeid);
                    ir.static_mem.alloc(name, typeid, type_size);
                }

                AstItem::Struct { .. } => {}

                AstItem::ForwardDecl { .. } | AstItem::ExternLib(_) => {}
            }
        }

        // if let Err(dupes) = crate::ir::ssa::verify_ssa(&ir) {
        //     println!("{}", ir);
        //     panic!("ir is not ssa, duplicates: {:?}", dupes);
        // }

        ir
    }
}

struct BlockBuilder<'ir, 'a, 's> {
    analyzer: &'a Analyzer<'s>,
    ir: &'ir mut IR<'s>,

    blocks: Vec<BasicBlock>,
    var_to_vreg: HashMap<&'s str, VirtualReg>,
    proc_args: HashMap<VirtualReg, u32>,
    size_map: HashMap<VirtualReg, ValSize>,
    stack: HashMap<VirtualReg, u32>,
    stack_size: u32,
    vreg_counter: u32,
    label_counter: u32,

    block_label: Label,
    // block_args: Vec<VirtualReg>,
    block_ops: Vec<Op>,
    block_decls: Vec<VirtualReg>,
}

impl<'ir, 'a, 's> BlockBuilder<'ir, 'a, 's> {
    pub fn new(
        analyzer: &'a Analyzer<'s>,
        ir: &'ir mut IR<'s>,
        initial_args: Vec<(&'s str, VirtualReg)>,
    ) -> Self {
        // let block_args = initial_args.iter().map(|(_, vreg)| *vreg).collect();
        let n_args = initial_args.len();
        // let stack = HashMap::from_iter(
        //     initial_args
        //         .iter()
        //         .enumerate()
        //         .map(|(i, (var, _))| (var.clone(), (i as u32) * 8)),
        // );
        let proc_args = HashMap::from_iter(
            initial_args
                .iter()
                .enumerate()
                .map(|(i, (_, vreg))| (*vreg, (i as u32))),
        );
        // let stack_size = (stack.len() as u32) * 8;

        Self {
            analyzer,
            ir,

            blocks: Vec::new(),
            var_to_vreg: initial_args.into_iter().collect(),
            proc_args,
            size_map: HashMap::new(),
            stack: HashMap::new(),
            stack_size: 0,
            vreg_counter: n_args as u32,
            label_counter: 0,

            block_label: Label::Entry,
            // block_args,
            block_ops: Vec::new(),
            block_decls: Vec::new(),
        }
    }

    pub fn build(
        mut self,
        block: Vec<Statement<'s, TypeId>>,
    ) -> (
        Vec<BasicBlock>,
        HashMap<VirtualReg, u32>,
        u32,
        HashMap<VirtualReg, ValSize>,
    ) {
        self.consume(block);
        self.commit_block(Terminator::Branch { label: Label::End }, Label::End);

        let mut run_again = true;
        while run_again {
            run_again = false;

            let mut succ_args = Vec::new();
            for bb in self.blocks.iter() {
                let mut args: HashSet<VirtualReg> = HashSet::new();
                for succ_label in bb.successors() {
                    if let Some(_args) = self
                        .blocks
                        .iter()
                        .find(|bb2| bb2.label == succ_label)
                        .map(|bb2| &bb2.args)
                    {
                        args.extend(_args.iter().filter(|vreg| !bb.decls.contains(vreg)));
                    }
                }

                succ_args.push(args);
            }

            for (bb, append_args) in self.blocks.iter_mut().zip(succ_args) {
                let len = bb.args.len();
                bb.args.extend(append_args);
                if bb.args.len() > len {
                    run_again = true;
                }
            }
        }

        // self.stack.extend(
        //     self.proc_args
        //         .into_iter()
        //         .enumerate()
        //         .map(|(i, vreg)| (vreg, (i as u32) + self.stack_size + 16)),
        // );

        (self.blocks, self.stack, self.stack_size, self.size_map)
    }

    fn consume(&mut self, block: Vec<Statement<'s, TypeId>>) {
        for stmt in block {
            match stmt {
                Statement::Declare { var, expr, .. } => {
                    assert!(
                        !self.var_to_vreg.contains_key(&var),
                        "variable declared twice"
                    );

                    if expr.typ == Some(TypeId::unit()) {
                        continue;
                    }

                    let size = ValSize::from_bytes(self.analyzer.types.size_of(expr.typ.unwrap()))
                        .unwrap();

                    let (dest, stack_offset) = self.get_or_insert_stack_var(var, size);
                    let src = self.flatten_expr(expr, Some(dest));

                    if src != SourceVal::VReg(dest) {
                        self.block_ops.push(Op::Assign { src, dest });
                        self.block_decls.push(dest);
                    }

                    self.block_ops.push(Op::Store {
                        src: SourceVal::VReg(dest),
                        stack_offset,
                    });
                }

                Statement::Assign { var, expr, .. } => {
                    let size = ValSize::from_bytes(self.analyzer.types.size_of(expr.typ.unwrap()))
                        .unwrap();
                    let src = self.flatten_expr(expr, None);

                    match var {
                        Assignable::Var(var) => {
                            if let Some((offset, typ)) = self.ir.static_mem.get(&var) {
                                let offset = *offset;
                                let typeid = typ.clone();

                                let ptr = self.get_vreg(ValSize::Doubleword);
                                let src = self.src_to_vreg(src);

                                self.block_ops.push(Op::Assign {
                                    src: SourceVal::StaticMem(offset),
                                    dest: ptr,
                                });

                                let size = self.analyzer.types.size_of(typeid);
                                if size <= 8 {
                                    self.block_ops.push(Op::StorePointer {
                                        src,
                                        ptr,
                                        size: ValSize::from_bytes(size).unwrap(),
                                        offset: 0,
                                    });
                                } else {
                                    todo!();
                                }
                            } else {
                                let (dest, stack_offset) = self.get_or_insert_stack_var(var, size);

                                self.block_ops.push(Op::Assign { src, dest });
                                self.block_ops.push(Op::Store {
                                    src: SourceVal::VReg(dest),
                                    stack_offset,
                                });
                            }
                        }
                        Assignable::Ptr(var, size) => {
                            let var_vreg = self.var_to_vreg.get(&var).copied().unwrap();
                            let src = self.src_to_vreg(src);
                            self.block_ops.push(Op::StorePointer {
                                src,
                                ptr: var_vreg,
                                size: size.unwrap(),
                                offset: 0,
                            });
                        }
                        Assignable::Index(array, index_expr, item_size) => {
                            let index = self.flatten_expr(*index_expr, None);
                            let index_vreg = self.src_to_vreg(index);

                            let array_vreg = self.get_vreg(ValSize::Doubleword);
                            self.load_var(&array, array_vreg);

                            let ptr = self.get_vreg(ValSize::Doubleword);

                            let src = self.src_to_vreg(src);

                            self.block_ops.push(Op::Add {
                                a: array_vreg,
                                b: index_vreg,
                                dest: ptr,
                            });

                            self.block_ops.push(Op::StorePointer {
                                src,
                                ptr,
                                size: item_size.unwrap(),
                                offset: 0,
                            });
                        }
                        Assignable::MemberAccess(parent, member) => {
                            let TypeKind::Pointer(value_typeid) =
                                self.analyzer.types.get(parent.typ.unwrap()).kind
                            else {
                                panic!()
                            };

                            let offset = self
                                .analyzer
                                .types
                                .offset_of_member(value_typeid, &member)
                                .unwrap();

                            let parent = self.flatten_expr(*parent, None);
                            let parent = self.src_to_vreg(parent);

                            let src = self.src_to_vreg(src);

                            self.block_ops.push(Op::StorePointer {
                                src,
                                ptr: parent,
                                size,
                                offset: offset as u32,
                            })
                        }
                    }
                }

                Statement::Expr(expr) => {
                    self.flatten_expr(expr, None);
                }

                Statement::If { guard, body } => {
                    let cond = self.flatten_expr(guard, None);
                    let cond = self.src_to_vreg(cond);

                    let if_true = self.next_label();
                    let if_false = self.next_label();

                    self.commit_block(
                        Terminator::BranchCond {
                            cond,
                            if_true,
                            if_false,
                        },
                        if_true,
                    );

                    self.consume(body);
                    self.commit_block(Terminator::Branch { label: if_false }, if_false);
                }
                Statement::Return(expr) => {
                    let value = self.flatten_expr(expr, None);
                    let value = self.src_to_vreg(value);
                    let new_label = self.next_label();
                    self.commit_block(Terminator::Return { value }, new_label);
                }
                Statement::WhileLoop { guard, body } => {
                    let guard_label = self.next_label();
                    let body_label = self.next_label();
                    let end_label = self.next_label();

                    self.commit_block(Terminator::Branch { label: guard_label }, guard_label);

                    let cond = self.flatten_expr(guard, None);
                    let cond = self.src_to_vreg(cond);
                    self.commit_block(
                        Terminator::BranchCond {
                            cond,
                            if_true: body_label,
                            if_false: end_label,
                        },
                        body_label,
                    );

                    self.consume(body);
                    self.commit_block(Terminator::Branch { label: guard_label }, end_label);
                }
            }
        }
    }

    fn commit_block(&mut self, terminator: Terminator, new_label: Label) {
        let label = std::mem::replace(&mut self.block_label, new_label);
        // let args = std::mem::take(&mut self.block_args);
        let ops = std::mem::take(&mut self.block_ops);

        let mut args = HashSet::new();
        if let Some(vreg) = terminator.vreg_used() {
            args.insert(vreg);
        }

        for op in ops.iter() {
            let (uses, assigned) = op.vregs_used();
            args.extend(uses);
            if let Some(assigned) = assigned {
                args.insert(assigned);
            }
        }

        for vreg in self.block_decls.iter() {
            args.remove(vreg);
        }

        let decls = std::mem::take(&mut self.block_decls);

        self.blocks.push(BasicBlock {
            label,
            args,
            ops,
            decls,
            terminator,
        });
    }

    fn next_label(&mut self) -> Label {
        let label = Label::Anon(self.label_counter);
        self.label_counter += 1;
        label
    }

    fn flatten_expr(
        &mut self,
        expr: Expression<'s, TypeId>,
        dest: Option<VirtualReg>,
    ) -> SourceVal {
        let type_size = self.analyzer.types.size_of(expr.typ.unwrap());
        let type_size = ValSize::from_bytes(type_size);

        match expr.inner {
            ExprInner::Const(num, explicit_type) => SourceVal::Immediate(num),
            ExprInner::Character(c) => SourceVal::Immediate(c as u64),
            ExprInner::String(string) => {
                let str_id = self.ir.insert_str_literal(string);
                SourceVal::String(str_id)
            }
            ExprInner::Bool(b) => SourceVal::Immediate(b as u64),

            ExprInner::Variable(var) => {
                let dest = self.get_vreg(type_size.unwrap());

                self.load_var(&var, dest);

                SourceVal::VReg(dest)
            }
            ExprInner::Pointer(var) => {
                // let stack_offset = self.get_stack_offset(&var);
                let val = *self.var_to_vreg.get(&var).unwrap();
                let dest = dest.unwrap_or_else(|| self.get_vreg(type_size.unwrap()));

                if let Some(&offset) = self.proc_args.get(&val) {
                    self.block_ops.push(Op::AddressOfArg { offset, dest });
                } else {
                    self.block_ops.push(Op::AddressOf { val, dest });
                }

                SourceVal::VReg(dest)
            }
            ExprInner::Deref(var, typ) => {
                let ptr = self.get_vreg(ValSize::Doubleword);
                let dest = dest.unwrap_or_else(|| self.get_vreg(type_size.unwrap()));

                self.load_var(&var, ptr);

                let size = self.analyzer.types.size_of(typ.unwrap());
                if size > 8 {
                    todo!()
                }
                self.block_ops.push(Op::LoadPointer {
                    ptr,
                    size: ValSize::from_bytes(size).unwrap(),
                    dest,
                });
                SourceVal::VReg(dest)
            }

            ExprInner::Arithmetic(expr1, expr2, op, _sign) => {
                // TODO: sign
                let a = self.flatten_expr(*expr1, None);
                let b = self.flatten_expr(*expr2, None);

                let a = self.src_to_vreg(a);
                let b = self.src_to_vreg(b);

                let dest = dest.unwrap_or_else(|| self.get_vreg(type_size.unwrap()));

                match op {
                    ArithmeticOp::Add => self.block_ops.push(Op::Add { a, b, dest }),
                    ArithmeticOp::Sub => self.block_ops.push(Op::Subtract { a, b, dest }),
                    ArithmeticOp::Mul => self.block_ops.push(Op::Multiply { a, b, dest }),
                    ArithmeticOp::Div => self.block_ops.push(Op::Divide { a, b, dest }),
                    ArithmeticOp::Mod => self.block_ops.push(Op::Modulo { a, b, dest }),
                }

                SourceVal::VReg(dest)
            }
            ExprInner::Comparison(expr1, expr2, op, sign) => {
                let expr1 = self.flatten_expr(*expr1, None);
                let expr2 = self.flatten_expr(*expr2, None);

                let expr1 = self.src_to_vreg(expr1);
                let expr2 = self.src_to_vreg(expr2);

                let dest = dest.unwrap_or_else(|| self.get_vreg(type_size.unwrap()));

                self.block_ops.push(Op::Compare {
                    a: expr1,
                    b: expr2,
                    cond: Condition::from_ast_op(op, matches!(sign, Some(Sign::Signed))),
                    dest,
                });

                SourceVal::VReg(dest)
            }
            ExprInner::Logical(lhs, rhs, op) => {
                let dest = dest.unwrap_or_else(|| self.get_vreg(type_size.unwrap()));

                let lhs = self.flatten_expr(*lhs, None);
                let lhs = self.src_to_vreg(lhs);

                let second_test = self.next_label();
                let test_true = self.next_label();
                let test_false = self.next_label();
                let end = self.next_label();

                let term = match op {
                    LogicalOp::And => Terminator::BranchCond {
                        cond: lhs,
                        if_true: second_test,
                        if_false: test_false,
                    },
                    LogicalOp::Or => Terminator::BranchCond {
                        cond: lhs,
                        if_true: test_true,
                        if_false: second_test,
                    },
                };

                self.commit_block(term, second_test);

                let rhs = self.flatten_expr(*rhs, None);
                let rhs = self.src_to_vreg(rhs);

                self.commit_block(
                    Terminator::BranchCond {
                        cond: rhs,
                        if_true: test_true,
                        if_false: test_false,
                    },
                    test_true,
                );

                self.block_decls.push(dest);
                self.block_ops.push(Op::Assign {
                    src: SourceVal::Immediate(1),
                    dest,
                });

                self.commit_block(Terminator::Branch { label: end }, test_false);

                self.block_decls.push(dest);
                self.block_ops.push(Op::Assign {
                    src: SourceVal::Immediate(0),
                    dest,
                });

                self.commit_block(Terminator::Branch { label: end }, end);

                SourceVal::VReg(dest)
            }

            ExprInner::Not(expr) => {
                let val = self.flatten_expr(*expr, dest);
                let val = self.src_to_vreg(val);
                let dest = dest.unwrap_or_else(|| self.get_vreg(type_size.unwrap()));

                self.block_ops.push(Op::Not { val, dest });

                SourceVal::VReg(dest)
            }

            ExprInner::Negate(expr) => {
                let val = self.flatten_expr(*expr, dest);
                let val = self.src_to_vreg(val);
                let dest = dest.unwrap_or_else(|| self.get_vreg(type_size.unwrap()));

                self.block_ops.push(Op::Negate { val, dest });

                SourceVal::VReg(dest)
            }

            ExprInner::Cast(expr, _typ) => self.flatten_expr(*expr, dest),

            ExprInner::Index(var, expr, item_size) => {
                let index = self.flatten_expr(*expr, None);
                let index_vreg = self.src_to_vreg(index);

                let array_vreg = self.get_vreg(ValSize::Doubleword);
                self.load_var(&var, array_vreg);

                let ptr = self.get_vreg(ValSize::Doubleword);

                let dest = dest.unwrap_or_else(|| self.get_vreg(type_size.unwrap()));

                self.block_ops.push(Op::Add {
                    a: array_vreg,
                    b: index_vreg,
                    dest: ptr,
                });

                self.block_ops.push(Op::LoadPointer {
                    ptr,
                    size: item_size.unwrap(),
                    dest,
                });

                SourceVal::VReg(dest)
            }

            ExprInner::MemberAccess(parent, member, typeid) => {
                let parent = self.flatten_expr(*parent, None);
                let parent_vreg = self.src_to_vreg(parent);

                let offset = self
                    .analyzer
                    .types
                    .offset_of_member(typeid.unwrap(), &member)
                    .unwrap();

                let dest = dest.unwrap_or_else(|| self.get_vreg(type_size.unwrap()));

                self.block_ops.push(Op::LoadOffset {
                    base: parent_vreg,
                    offset: offset as u32,
                    dest,
                });

                SourceVal::VReg(dest)
            }

            ExprInner::FnCall(function, args) => {
                let args = args
                    .into_iter()
                    .map(|e| {
                        let src = self.flatten_expr(e, None);
                        self.src_to_vreg(src)
                    })
                    .collect();

                let dest = dest.or_else(|| type_size.map(|s| self.get_vreg(s)));

                self.block_ops.push(Op::Call {
                    function: function.to_owned(),
                    args,
                    dest,
                });

                dest.map(SourceVal::VReg).unwrap_or(SourceVal::Immediate(0))
            }

            ExprInner::SizeOf(typ) => {
                let size = self.analyzer.types.size_of(typ);
                SourceVal::Immediate(size)
            }
        }
    }

    fn load_var(&mut self, var: &str, dest: VirtualReg) {
        if let Some((offset, typeid)) = self.ir.static_mem.get(var).copied() {
            let ptr = self.get_vreg(ValSize::Doubleword);
            self.block_ops.push(Op::Assign {
                src: SourceVal::StaticMem(offset),
                dest: ptr,
            });

            let size = self.analyzer.types.size_of(typeid);
            if size > 8 {
                todo!()
            }
            self.block_ops.push(Op::LoadPointer {
                ptr,
                size: ValSize::from_bytes(size).unwrap(),
                dest,
            });

            return;
        }

        let vreg = *self.var_to_vreg.get(var).unwrap();

        if let Some(&offset) = self.proc_args.get(&vreg) {
            self.block_ops.push(Op::LoadArg { offset, dest });
        } else if let Some(offset) = self.ir.static_mem.get(var) {
            todo!()
        } else {
            let offset = self.get_stack_offset(var);
            self.block_ops.push(Op::Load {
                stack_offset: offset,
                dest,
            });
        }
    }

    fn get_or_insert_stack_var(&mut self, var: &'s str, size: ValSize) -> (VirtualReg, u32) {
        if self.ir.static_mem.get(var).is_some() {
            panic!()
        }

        let vreg = self.var_to_vreg.get(var).copied().unwrap_or_else(|| {
            let vreg = self.get_vreg(size);
            self.var_to_vreg.insert(var, vreg);
            vreg
        });

        let offset = self.stack.get(&vreg).copied().unwrap_or_else(|| {
            self.stack.insert(vreg, self.stack_size);
            self.stack_size += 8;
            self.stack_size - 8
        });

        (vreg, offset)
    }

    fn get_stack_offset(&self, var: &str) -> u32 {
        let vreg = self
            .var_to_vreg
            .get(var)
            .unwrap_or_else(|| panic!("undefined variable '{}'", var));

        *self
            .stack
            .get(vreg)
            .unwrap_or_else(|| panic!("variable {} is not on the stack", var))
    }

    fn get_vreg(&mut self, size: ValSize) -> VirtualReg {
        let vreg = VirtualReg(self.vreg_counter);
        self.vreg_counter += 1;
        self.block_decls.push(vreg);
        self.size_map.insert(vreg, size);
        vreg
    }

    fn src_to_vreg(&mut self, src: SourceVal) -> VirtualReg {
        match src {
            SourceVal::Immediate(_) | SourceVal::String(_) | SourceVal::StaticMem(_) => {
                let dest = self.get_vreg(ValSize::Doubleword);
                self.block_ops.push(Op::Assign { src, dest });
                dest
            }
            SourceVal::VReg(vreg) => vreg,
        }
    }
}

// struct _BlockBuilder<'ir> {
//     ir: &'ir mut IR,
//     vregs: HashMap<String, VirtualReg>,
//     vreg_counter: u32,
//     labels: HashMap<OpIndex, Vec<Label>>,
//     label_counter: u32,
//     ops: Vec<Op>,
// }
//
// impl<'ir> _BlockBuilder<'ir> {
//     pub fn new(ir: &'ir mut IR) -> Self {
//         Self {
//             vregs: HashMap::new(),
//             vreg_counter: 0,
//             labels: HashMap::new(),
//             label_counter: 0,
//             ops: Vec::new(),
//             ir,
//         }
//     }
//
//     pub fn build(mut self, block: Vec<Statement>) -> BasicBlock {
//         self.consume_block(block);
//         BasicBlock {
//             ops: self.ops,
//             labels: self.labels,
//         }
//     }
//
//     fn consume_block(&mut self, block: Vec<Statement>) {
//         self.ops.reserve(block.len());
//
//         for stmt in block {
//             match stmt {
//                 Statement::Declare { var, expr, .. } => {
//                     assert!(!self.vregs.contains_key(&var), "variable declared twice");
//
//                     let dest = self.get_or_insert_vreg(var);
//                     let src = self.unroll_expr(expr, Some(dest));
//
//                     if src != SourceVal::VReg(dest) {
//                         self.ops.push(Op::Assign { src, dest });
//                     }
//                 }
//                 Statement::Assign { var, expr, .. } => {
//                     let dest = self.get_or_insert_vreg(var.symbol());
//                     let src = self.unroll_expr(expr, Some(dest));
//
//                     match var {
//                         Assignable::Var(_) => {
//                             if src.reg() != Some(dest) {
//                                 self.ops.push(Op::Assign { src, dest })
//                             }
//                         }
//                         Assignable::Ptr(_) => {
//                             let src = self.src_to_vreg(src);
//                             self.ops.push(Op::StorePointer { src, ptr: dest });
//                         }
//                     }
//                 }
//                 Statement::Return(expr) => {
//                     let value = self.unroll_expr(expr, None);
//                     self.ops.push(Op::Return { value });
//                 }
//
//                 Statement::If { guard, body } => {
//                     let cond = self.unroll_expr(guard, None);
//                     let cond = self.src_to_vreg(cond);
//                     let label = self.reserve_label();
//                     self.ops.push(Op::BranchIfNot { cond, label });
//
//                     let outer_vregs = self.vregs.clone();
//                     let outer_vreg_counter = self.vreg_counter;
//
//                     self.consume_block(body);
//
//                     for (name, inner) in self.vregs.iter() {
//                         if let Some(outer) = outer_vregs.get(name)
//                             && inner != outer
//                         {
//                             self.ops.push(Op::Assign {
//                                 src: SourceVal::VReg(*inner),
//                                 dest: *outer,
//                             });
//                         }
//                     }
//
//                     self.set_label_here(label);
//
//                     self.vregs = outer_vregs;
//                     self.vreg_counter = outer_vreg_counter;
//                 }
//
//                 Statement::WhileLoop { guard, body } => {
//                     let cond_label = self.reserve_label();
//
//                     let outer_vregs = self.vregs.clone();
//                     let outer_vreg_counter = self.vreg_counter;
//
//                     self.ops.push(Op::Branch { label: cond_label });
//
//                     let body_label = self.insert_label();
//                     self.consume_block(body);
//
//                     self.set_label_here(cond_label);
//                     let cond = self.unroll_expr(guard, None);
//                     let cond = self.src_to_vreg(cond);
//                     self.ops.push(Op::BranchIf {
//                         cond,
//                         label: body_label,
//                     });
//
//                     for (name, inner) in self.vregs.iter() {
//                         if let Some(outer) = outer_vregs.get(name)
//                             && inner != outer
//                         {
//                             self.ops.push(Op::Assign {
//                                 src: SourceVal::VReg(*inner),
//                                 dest: *outer,
//                             });
//                         }
//                     }
//
//                     self.vregs = outer_vregs;
//                     self.vreg_counter = outer_vreg_counter;
//                 }
//
//                 Statement::Expr(expr) => {
//                     self.unroll_expr(expr, None);
//                 }
//             }
//         }
//     }
//
//     fn unroll_expr(&mut self, expr: Expression, dest: Option<VirtualReg>) -> SourceVal {
//         match expr.inner {
//             ExprInner::Const(num) => SourceVal::Immediate(num),
//             ExprInner::Character(c) => SourceVal::Immediate(c as i64),
//             ExprInner::String(string) => {
//                 let str_id = self.ir.alloc_str_literal(string);
//                 SourceVal::String(str_id)
//             }
//             ExprInner::Bool(b) => SourceVal::Immediate(b as i64),
//
//             ExprInner::Variable(var) => SourceVal::VReg(self.expect_vreg(&var)),
//             ExprInner::Pointer(var) => {
//                 let val = self.expect_vreg(&var);
//                 let dest = dest.unwrap_or_else(|| self.get_vreg());
//
//                 self.ops.push(Op::AddressOf { val, dest });
//                 SourceVal::VReg(dest)
//             }
//             ExprInner::Deref(var, typ) => {
//                 let ptr = self.expect_vreg(&var);
//                 let dest = dest.unwrap_or_else(|| self.get_vreg());
//
//                 self.ops.push(Op::LoadPointer {
//                     ptr,
//                     size: typ.unwrap().size(),
//                     dest,
//                 });
//                 SourceVal::VReg(dest)
//             }
//
//             ExprInner::Arithmetic(expr1, expr2, op, _sign) => {
//                 // TODO: sign
//                 let a = self.unroll_expr(*expr1, None);
//                 let b = self.unroll_expr(*expr2, None);
//
//                 let a = self.src_to_vreg(a);
//                 let b = self.src_to_vreg(b);
//
//                 let dest = dest.unwrap_or_else(|| self.get_vreg());
//
//                 match op {
//                     ArithmeticOp::Add => self.ops.push(Op::Add { a, b, dest }),
//                     ArithmeticOp::Sub => self.ops.push(Op::Subtract { a, b, dest }),
//                     ArithmeticOp::Mult => self.ops.push(Op::Multiply { a, b, dest }),
//                     ArithmeticOp::Div => self.ops.push(Op::Divide { a, b, dest }),
//                 }
//
//                 SourceVal::VReg(dest)
//             }
//             ExprInner::Comparison(expr1, expr2, op, sign) => {
//                 let expr1 = self.unroll_expr(*expr1, None);
//                 let expr2 = self.unroll_expr(*expr2, None);
//
//                 let expr1 = self.src_to_vreg(expr1);
//                 let expr2 = self.src_to_vreg(expr2);
//
//                 let dest = dest.unwrap_or_else(|| self.get_vreg());
//
//                 self.ops.push(Op::Compare {
//                     a: expr1,
//                     b: expr2,
//                     cond: Condition::from_ast_op(op, matches!(sign, Some(Sign::Signed))),
//                     dest,
//                 });
//
//                 SourceVal::VReg(dest)
//             }
//
//             ExprInner::FnCall(function, args) => {
//                 let args = args
//                     .into_iter()
//                     .map(|e| {
//                         let src = self.unroll_expr(e, None);
//                         self.src_to_vreg(src)
//                     })
//                     .collect();
//
//                 let dest = dest.unwrap_or_else(|| self.get_vreg());
//
//                 println!("call to {} ret {:?}", function, dest);
//                 self.ops.push(Op::Call {
//                     function: function.clone(),
//                     args,
//                     dest: Some(dest),
//                 });
//
//                 SourceVal::VReg(dest)
//             }
//
//             ExprInner::Cast(expr, _typ) => self.unroll_expr(*expr, dest),
//         }
//     }
//
//     fn get_or_insert_vreg<S: Into<String> + AsRef<str>>(&mut self, var: S) -> VirtualReg {
//         if let Some(&vreg) = self.vregs.get(var.as_ref()) {
//             vreg
//         } else {
//             let vreg = self.get_vreg();
//             self.vregs.insert(var.into(), vreg);
//             vreg
//         }
//     }
//
//     fn expect_vreg(&self, var: &str) -> VirtualReg {
//         *self
//             .vregs
//             .get(var)
//             .unwrap_or_else(|| panic!("undefined variable '{}'", var))
//     }
//
//     fn get_vreg(&mut self) -> VirtualReg {
//         let vreg = VirtualReg(self.vreg_counter);
//         self.vreg_counter += 1;
//         vreg
//     }
//
//     fn src_to_vreg(&mut self, src: SourceVal) -> VirtualReg {
//         match src {
//             SourceVal::Immediate(_) | SourceVal::String(_) => {
//                 let dest = self.get_vreg();
//                 self.ops.push(Op::Assign { src, dest });
//                 dest
//             }
//             SourceVal::VReg(vreg) => vreg,
//         }
//     }
//
//     fn reserve_label(&mut self) -> Label {
//         self.label_counter += 1;
//         Label::Anon(self.label_counter - 1)
//     }
//
//     fn set_label_here(&mut self, label: Label) {
//         self.labels.entry(self.ops.len()).or_default().push(label);
//     }
//
//     fn insert_label(&mut self) -> Label {
//         let label = self.reserve_label();
//         self.set_label_here(label);
//         label
//     }
// }
