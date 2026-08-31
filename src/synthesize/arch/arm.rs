use std::assert_matches;
use std::collections::HashMap;

use colored::Colorize;
use ux::{i7, i12, i19, i26, u9, u12, u48};

use crate::{
    ir::{
        BasicBlock, Condition, IR, Item, Label, Op, SourceVal, StrId, Terminator, ValSize,
        VirtualReg,
    },
    synthesize::arch::{
        Assembler, LinkableCode, MachineCode,
        arm::{
            builtin::SyscallType,
            instr::{
                AddImmVal, BranchOffset, EitherOffset, EitherReg, FnOffset, ImmShift16, Inst,
                Instruction, PageAddr,
            },
            reg::Register,
        },
    },
};

pub mod builtin;
pub mod instr;
pub mod reg;

// const MAX_EXIT_CODE: u16 = 255; // On UNIX

type InstrIndex = usize;

#[derive(Default)]
pub struct ArmAssembler {
    code: MachineCode,
    functions: HashMap<String, InstrIndex>,
    fn_calls: Vec<(String, InstrIndex)>,
    stacks: Vec<i12>,
    str_literal_offsets: HashMap<StrId, usize>,

    lazy_emitters: Vec<Box<dyn Fn(&mut ArmAssembler, usize)>>,
    instructions: Vec<Inst<Register>>,
    // bb_args: Vec<(usize, Vec<VirtualReg>)>,
    empty_label_map: HashMap<Label, i32>,
}

impl Assembler for ArmAssembler {
    fn assemble(ir: IR, main_fn: &str) -> LinkableCode<Self> {
        let mut assembler = ArmAssembler::default();

        let mut str_offset = 0;
        for (string, id) in ir.strings {
            assembler.str_literal_offsets.insert(id, str_offset);
            str_offset += string.len() + 1; // with zero-byte
            assembler.code.str_literals.push(string);
        }

        for item in ir.items {
            let Item::Function {
                name,
                args,
                stack,
                stack_size,
                size_map,
                body,
            } = item;

            let offset = assembler.instructions.len() as u64;
            // TODO: name as reference instead of owned
            assembler
                .code
                .symbols
                .push((name.clone().into_owned(), offset * 4));
            assembler
                .functions
                .insert(name.into_owned(), offset as usize);

            ProcedureGen::assemble(&mut assembler, stack, stack_size, &size_map, body);
        }

        builtin::assemble(&mut assembler);

        let entry_offset = (assembler.instructions.len() * 4) as u64;
        assembler
            .code
            .symbols
            .push((String::from("_entry_point"), entry_offset));
        assembler.code.entry_point_offset = entry_offset;

        assembler.emit_many([
            Inst::BranchLink {
                offset: FnOffset::Fixed(
                    *assembler
                        .functions
                        .get(main_fn)
                        .unwrap_or_else(|| panic!("main function {} not found", main_fn))
                        as i32
                        - (entry_offset / 4) as i32,
                ),
            },
            Inst::Movz {
                shift: ImmShift16::L0,
                value: SyscallType::Exit as u16,
                dest: Register::X16,
            },
            Inst::Syscall,
        ]);

        LinkableCode(assembler)
    }

    fn code_size(&self) -> usize {
        self.instructions.len() * 4
    }

    fn str_literals(&self) -> &[String] {
        &self.code.str_literals
    }

    fn into_machine_code(mut self, str_table_offset: usize, bss_offset: u64) -> MachineCode {
        self.code
            .instructions
            .extend(
                self.instructions
                    .into_iter()
                    .enumerate()
                    .flat_map(|(i, mut inst)| {
                        inst.link(i as i32, &self.functions, str_table_offset, bss_offset);
                        inst.encode().to_le_bytes()
                    }),
            );

        self.code
    }
}

impl ArmAssembler {
    pub fn emit(&mut self, inst: Inst<Register>) {
        self.instructions.push(inst);
    }

    pub fn emit_many(&mut self, insts: impl IntoIterator<Item = Inst<Register>>) {
        self.instructions.extend(insts);
    }
}

#[derive(Debug, Clone, Copy)]
pub enum InstMarker {
    None,
    FirstInBB(Label),
    // Terminator(Label, Terminator),
}

#[derive(Default)]
struct ProcedureGen {
    instructions: Vec<(Inst<VirtualReg>, InstMarker)>,
}

impl ProcedureGen {
    pub fn assemble(
        assembler: &mut ArmAssembler,
        stack: HashMap<VirtualReg, u32>,
        stack_size: u32,
        size_map: &HashMap<VirtualReg, ValSize>,
        body: Vec<BasicBlock>,
    ) {
        let mut proc = ProcedureGen::default();

        for bb in body {
            let first_op_index = proc.instructions.len();
            for op in bb.ops {
                match op {
                    Op::Assign { src, dest } => {
                        match src {
                            SourceVal::Immediate(imm) => {
                                proc.emit(Inst::Movz {
                                    shift: instr::ImmShift16::L0,
                                    value: imm as u16,
                                    dest,
                                });

                                if imm > u16::MAX as u64 {
                                    proc.emit(Inst::Movk {
                                        shift: ImmShift16::L16,
                                        value: (imm >> 16) as u16,
                                        dest,
                                    });
                                }

                                if imm > u32::MAX as u64 {
                                    proc.emit(Inst::Movk {
                                        shift: ImmShift16::L32,
                                        value: (imm >> 32) as u16,
                                        dest,
                                    });
                                }

                                if imm > u48::MAX.into() {
                                    proc.emit(Inst::Movk {
                                        shift: ImmShift16::L48,
                                        value: (imm >> 48) as u16,
                                        dest,
                                    });
                                }
                            }
                            SourceVal::VReg(src) => proc.emit(Inst::Mov {
                                src: EitherReg::Virt(src),
                                dest: EitherReg::Virt(dest),
                            }),
                            SourceVal::String(str_id) => {
                                let rel_str_offset =
                                    *assembler.str_literal_offsets.get(&str_id).unwrap_or_else(
                                        || panic!("no string found for str_id #{}", str_id),
                                    );

                                // TODO: handle case where code is longer than one page, need to
                                // link by pc-relative index
                                proc.emit_many([
                                    Inst::Adrp {
                                        page_addr: PageAddr::String(rel_str_offset),
                                        dest,
                                    },
                                    Inst::AddImm {
                                        a: EitherReg::Virt(dest),
                                        imm: AddImmVal::String(rel_str_offset),
                                        dest: EitherReg::Virt(dest),
                                    },
                                ]);
                            }
                            SourceVal::StaticMem(offset) => proc.emit_many([
                                Inst::Adrp {
                                    page_addr: PageAddr::Bss(offset),
                                    dest,
                                },
                                Inst::AddImm {
                                    a: EitherReg::Virt(dest),
                                    imm: AddImmVal::Bss(offset),
                                    dest: EitherReg::Virt(dest),
                                },
                            ]),
                        }
                    }
                    Op::Store { src, stack_offset } => {
                        let SourceVal::VReg(source) = src else {
                            panic!()
                        };

                        let size = *size_map.get(&source).unwrap();

                        proc.emit(Inst::Store {
                            source,
                            base: EitherReg::Phys(Register::SP),
                            offset: EitherOffset::Imm(u12::new(
                                stack_offset as u16 / size.to_bytes() as u16,
                            )),
                            size,
                        });
                    }
                    Op::Load { stack_offset, dest } => {
                        let size = *size_map.get(&dest).unwrap();

                        proc.emit(Inst::Load {
                            base: EitherReg::Phys(Register::SP),
                            offset: EitherOffset::Imm(u12::new(
                                stack_offset as u16 / size.to_bytes() as u16,
                            )),
                            dest,
                            size,
                        });
                    }
                    Op::LoadOffset { base, offset, dest } => {
                        let size = *size_map.get(&dest).unwrap();

                        proc.emit(Inst::Load {
                            base: EitherReg::Virt(base),
                            offset: EitherOffset::Imm(u12::new(
                                offset as u16 / size.to_bytes() as u16,
                            )),
                            dest,
                            size,
                        });
                    }
                    Op::LoadArg { offset, dest } => {
                        proc.emit(Inst::Load {
                            base: EitherReg::Phys(Register::FP),
                            offset: EitherOffset::Imm(u12::new(2 + offset as u16)),
                            dest,
                            size: *size_map.get(&dest).unwrap(),
                        });
                    }
                    Op::AddressOf { val, dest } => {
                        let offset = *stack.get(&val).unwrap_or_else(|| {
                            panic!("cannot take address of temporary virtual register {}", val)
                        });

                        proc.emit(Inst::AddImm {
                            a: EitherReg::Phys(Register::SP),
                            imm: AddImmVal::Fixed(u12::new(offset as u16)),
                            dest: EitherReg::Virt(dest),
                        });
                    }
                    Op::AddressOfArg { offset, dest } => proc.emit(Inst::AddImm {
                        a: EitherReg::Phys(Register::FP),
                        imm: AddImmVal::Fixed(u12::new(16 + offset as u16)),
                        dest: EitherReg::Virt(dest),
                    }),
                    Op::LoadPointer { ptr, size, dest } => proc.emit(Inst::Load {
                        base: EitherReg::Virt(ptr),
                        offset: EitherOffset::Imm(u12::new(0)),
                        dest,
                        size,
                    }),
                    Op::StorePointer {
                        src,
                        ptr,
                        size,
                        offset,
                    } => {
                        proc.emit(Inst::Store {
                            source: src,
                            base: EitherReg::Virt(ptr),
                            offset: EitherOffset::Imm(u12::new(
                                offset as u16 / size.to_bytes() as u16,
                            )),
                            size,
                        });
                    }

                    Op::Add { a, b, dest } => proc.emit(Inst::Add { a, b, dest }),
                    Op::Subtract { a, b, dest } => proc.emit(Inst::Sub { a, b, dest }),
                    Op::Multiply { a, b, dest } => proc.emit(Inst::Mul { a, b, dest }),
                    Op::Divide { a, b, dest } => proc.emit(Inst::Div {
                        a,
                        b,
                        dest,
                        signed: true,
                    }),
                    Op::Modulo { a, b, dest } => {
                        proc.emit_many([
                            Inst::Div {
                                a,
                                b,
                                dest,
                                signed: true,
                            },
                            Inst::Mul { a: dest, b, dest },
                            Inst::Sub { a, b: dest, dest },
                        ]);
                    }

                    Op::Not { val, dest } => proc.emit_many([
                        Inst::CmpImm {
                            val,
                            imm: i12::new(0),
                        },
                        Inst::Cset {
                            inv_cond: Condition::NotEqual,
                            dest,
                        },
                    ]),

                    Op::Negate { val, dest } => proc.emit(Inst::Neg { val, dest }),

                    Op::Compare { a, b, cond, dest } => {
                        proc.emit_many([
                            Inst::Cmp { a, b },
                            Inst::Cset {
                                inv_cond: cond.inverted(),
                                dest,
                            },
                        ]);
                    }

                    Op::Select { a, b, cond, dest } => {
                        todo!()
                    }

                    Op::Call {
                        function,
                        args,
                        dest,
                    } => {
                        // save regs
                        // push args to stack
                        // call fn

                        let arg_stack_size = ((8 * args.len() as u32 + 15) >> 4) << 4;
                        let arg_stack_size_u12 = u12::new(arg_stack_size as u16);

                        proc.emit_many([
                            Inst::BeginFnCall {
                                reserved_stack_size: arg_stack_size,
                            },
                            Inst::SubImm {
                                a: EitherReg::Phys(Register::SP),
                                imm: arg_stack_size_u12,
                                dest: EitherReg::Phys(Register::SP),
                            },
                        ]);

                        for (i, arg) in args.iter().enumerate() {
                            proc.emit(Inst::Store {
                                source: *arg,
                                base: EitherReg::Phys(Register::SP),
                                offset: EitherOffset::Imm(u12::new(i as u16)),
                                size: ValSize::Doubleword,
                            });
                        }

                        proc.emit(Inst::BranchLink {
                            offset: FnOffset::Dynamic(function),
                        });

                        proc.emit_many([
                            Inst::AddImm {
                                a: EitherReg::Phys(Register::SP),
                                imm: AddImmVal::Fixed(arg_stack_size_u12),
                                dest: EitherReg::Phys(Register::SP),
                            },
                            Inst::EndFnCall,
                        ]);

                        if let Some(dest) = dest {
                            proc.emit(Inst::Mov {
                                src: EitherReg::Phys(Register::X0),
                                dest: EitherReg::Virt(dest),
                            });
                        }
                    }
                }
            }

            match bb.terminator {
                Terminator::Branch { label } => proc.emit(Inst::Branch {
                    offset: BranchOffset::Dynamic(label),
                }),
                Terminator::BranchCond {
                    cond,
                    if_true,
                    if_false,
                } => proc.emit_many([
                    Inst::BranchZero {
                        offset: BranchOffset::Dynamic(if_false),
                        reg: cond,
                    },
                    Inst::Branch {
                        offset: BranchOffset::Dynamic(if_true),
                    },
                ]),

                Terminator::Return { value } => proc.emit_many([
                    Inst::Mov {
                        src: EitherReg::Virt(value),
                        dest: EitherReg::Phys(Register::X0),
                    },
                    Inst::Branch {
                        offset: BranchOffset::Dynamic(Label::End),
                    },
                ]),
            }

            proc.instructions[first_op_index].1 = InstMarker::FirstInBB(bb.label);
        }

        let (instructions, extra_stack_size) =
            reg::allocate(proc.instructions, stack_size as u16, size_map);

        let mut label_map = HashMap::from_iter([(Label::End, instructions.len() as i32)]);
        for (i, (_, marker)) in instructions.iter().enumerate() {
            match marker {
                InstMarker::FirstInBB(label) => {
                    label_map.insert(*label, i as i32);
                }
                InstMarker::None => {}
            }
        }

        // align to 16 bytes
        let stack_size = ((stack_size as u16 + extra_stack_size + 15) >> 4) << 4;

        assembler.emit_many([
            Inst::StorePair {
                base: Register::SP,
                first: Register::FP,
                second: Register::LR,
                offset: i7::new(-2),
            },
            Inst::Mov {
                src: EitherReg::Phys(Register::SP),
                dest: EitherReg::Phys(Register::FP),
            },
        ]);

        if stack_size > 0 {
            assembler.emit(Inst::SubImm {
                a: EitherReg::Phys(Register::SP),
                imm: u12::new(stack_size),
                dest: EitherReg::Phys(Register::SP),
            });
        }

        assembler
            .instructions
            .extend(
                instructions
                    .into_iter()
                    .enumerate()
                    .map(|(i, (mut inst, _))| {
                        inst.fix_labels(i as i32, &label_map);
                        inst
                    }),
            );

        if stack_size > 0 {
            assembler.emit(Inst::AddImm {
                a: EitherReg::Phys(Register::SP),
                imm: AddImmVal::Fixed(u12::new(stack_size)),
                dest: EitherReg::Phys(Register::SP),
            });
        }
        assembler.emit_many([
            Inst::LoadPair {
                base: Register::SP,
                first: Register::FP,
                second: Register::LR,
                offset: i7::new(2),
            },
            Inst::Ret {
                value: Register::X0,
            },
        ]);
    }

    fn emit(&mut self, inst: Inst<VirtualReg>) {
        self.instructions.push((inst, InstMarker::None));
    }

    fn emit_many(&mut self, insts: impl IntoIterator<Item = Inst<VirtualReg>>) {
        self.instructions
            .extend(insts.into_iter().map(|inst| (inst, InstMarker::None)));
    }
}
