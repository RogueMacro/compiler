use itertools::Itertools;
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    fmt,
};

use crate::analyze::{
    ast::CompareOp,
    semantics::types::{ParsedType, TypeId},
};

pub mod codegen;
pub mod lifetime;
pub mod ssa;

#[derive(Default)]
pub struct IR<'s> {
    pub items: Vec<Item<'s>>,
    pub strings: HashMap<String, StrId>,
    pub static_mem: StaticMemory<'s>,
}

impl<'s> IR<'s> {
    pub fn insert_str_literal(&mut self, string: String) -> StrId {
        let len = self.strings.len();
        *self.strings.entry(string).or_insert(len)
    }
}

#[derive(Default)]
pub struct StaticMemory<'s> {
    allocs: HashMap<&'s str, (u64, TypeId)>,
    size: u64,
}

impl<'s> StaticMemory<'s> {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn alloc(&mut self, name: &'s str, typ: TypeId, type_size: u64) -> u64 {
        let offset = self.size;
        self.size += type_size;
        self.allocs.insert(name, (offset, typ));
        offset
    }

    pub fn get(&self, name: &str) -> Option<&(u64, TypeId)> {
        self.allocs.get(name)
    }
}

pub type StrId = usize;
pub type OpIndex = usize;

pub enum Item<'s> {
    Function {
        name: Cow<'s, str>,
        args: Vec<VirtualReg>,
        stack: HashMap<VirtualReg, u32>,
        stack_size: u32,
        size_map: HashMap<VirtualReg, ValSize>,
        body: Vec<BasicBlock>,
    },
}

/// A [basic block](BasicBlock) is a sequence of operations that do not affect control flow. Each basic block
/// contains exactly one [terminator](Terminator), which specifies the next block in the control flow graph (CFG).
/// Additionally basic blocks can specify arguments, which allows it to use registers passed on from
/// other basic blocks. This is an alternative to LLVMs phi nodes.
///
/// The next basic block is called the successor, and the previous is called the predecessor. A basic block
/// can have multiple succesors and predecessors.
pub struct BasicBlock {
    pub label: Label,
    pub args: HashSet<VirtualReg>,
    pub ops: Vec<Operation>,
    pub decls: Vec<VirtualReg>,
    pub terminator: Terminator,
}

impl BasicBlock {
    pub fn successors(&self) -> Vec<Label> {
        match self.terminator {
            Terminator::Branch { label } => vec![label],
            Terminator::BranchCond {
                if_true, if_false, ..
            } => vec![if_true, if_false],
            Terminator::Return { .. } => vec![Label::End],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Label {
    Entry,
    Anon(u32),
    End,
}

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Entry => write!(f, ".entry"),
            Self::Anon(n) => write!(f, ".L{}", n),
            Self::End => write!(f, ".end"),
        }
    }
}

pub type Op = Operation;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ValSize {
    Byte = 0,
    Halfword = 1,
    Word = 2,
    Doubleword = 3,
}

impl ValSize {
    pub fn from_bytes(size: u64) -> Option<Self> {
        match size {
            1 => Some(Self::Byte),
            2 => Some(Self::Halfword),
            4 => Some(Self::Word),
            8 => Some(Self::Doubleword),
            _ => None,
        }
    }

    pub fn to_bytes(&self) -> u64 {
        match self {
            ValSize::Byte => 1,
            ValSize::Halfword => 2,
            ValSize::Word => 4,
            ValSize::Doubleword => 8,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Terminator {
    Branch {
        label: Label,
    },
    BranchCond {
        cond: VirtualReg,
        if_true: Label,
        if_false: Label,
    },
    Return {
        value: VirtualReg,
    },
}

impl Terminator {
    pub fn vreg_used(&self) -> Option<VirtualReg> {
        match self {
            Terminator::Branch { .. } => None,
            Terminator::BranchCond { cond, .. } => Some(*cond),
            Terminator::Return { value } => Some(*value),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Operation {
    Assign {
        src: SourceVal,
        dest: VirtualReg,
    },
    Store {
        src: SourceVal,
        stack_offset: u32,
    },
    Load {
        stack_offset: u32,
        dest: VirtualReg,
    },
    LoadOffset {
        base: VirtualReg,
        offset: u32,
        dest: VirtualReg,
    },
    LoadArg {
        offset: u32,
        dest: VirtualReg,
    },
    AddressOf {
        val: VirtualReg,
        dest: VirtualReg,
    },
    AddressOfArg {
        offset: u32,
        dest: VirtualReg,
    },
    LoadPointer {
        ptr: VirtualReg,
        size: ValSize,
        dest: VirtualReg,
    },
    StorePointer {
        src: VirtualReg,
        ptr: VirtualReg,
        size: ValSize,
        offset: u32,
    },
    Add {
        a: VirtualReg,
        b: VirtualReg,
        dest: VirtualReg,
    },
    Subtract {
        a: VirtualReg,
        b: VirtualReg,
        dest: VirtualReg,
    },
    Multiply {
        a: VirtualReg,
        b: VirtualReg,
        dest: VirtualReg,
    },
    Divide {
        a: VirtualReg,
        b: VirtualReg,
        dest: VirtualReg,
    },
    Modulo {
        a: VirtualReg,
        b: VirtualReg,
        dest: VirtualReg,
    },
    Negate {
        val: VirtualReg,
        dest: VirtualReg,
    },
    Not {
        val: VirtualReg,
        dest: VirtualReg,
    },
    Compare {
        a: VirtualReg,
        b: VirtualReg,
        cond: Condition,
        dest: VirtualReg,
    },
    Select {
        a: u32,
        b: u32,
        cond: VirtualReg,
        dest: VirtualReg,
    },
    Call {
        function: String,
        args: Vec<VirtualReg>,
        dest: Option<VirtualReg>,
    },
}

impl Operation {
    pub fn _vregs_used(&self, _out: &mut Vec<VirtualReg>) {}

    /// Gets the virtual registers used in this operation. Both source and destination registers.
    pub fn vregs_used(&self) -> (HashSet<VirtualReg>, Option<VirtualReg>) {
        let mut used = HashSet::new();
        let mut assigned = None;

        let mut push = |vreg: Option<VirtualReg>| {
            if let Some(vreg) = vreg {
                used.insert(vreg);
            }
        };

        match self {
            Operation::Assign { src, dest } => {
                push(src.reg());
                assigned = Some(*dest);
            }
            Operation::Store { src, stack_offset } => {
                push(src.reg());
            }
            Operation::Load { stack_offset, dest } => {
                assigned = Some(*dest);
            }
            Operation::LoadOffset { base, offset, dest } => {
                assigned = Some(*dest);
                push(Some(*base));
            }
            Operation::LoadArg { offset, dest } => {
                assigned = Some(*dest);
            }
            Operation::AddressOf { val: _, dest } => {
                push(Some(*dest));
            }
            Operation::AddressOfArg { offset, dest } => {
                push(Some(*dest));
            }
            Operation::LoadPointer { ptr, size: _, dest } => {
                push(Some(*ptr));
                assigned = Some(*dest);
            }
            Operation::StorePointer {
                src,
                ptr,
                size: _,
                offset: _,
            } => {
                push(Some(*src));
                push(Some(*ptr));
            }

            Operation::Add { a, b, dest }
            | Operation::Subtract { a, b, dest }
            | Operation::Multiply { a, b, dest }
            | Operation::Divide { a, b, dest }
            | Operation::Modulo { a, b, dest } => {
                push(Some(*a));
                push(Some(*b));
                assigned = Some(*dest);
            }

            Operation::Not { val, dest } | Operation::Negate { val, dest } => {
                push(Some(*val));
                assigned = Some(*dest);
            }

            Operation::Compare {
                a,
                b,
                cond: _,
                dest,
            } => {
                push(Some(*a));
                push(Some(*b));
                assigned = Some(*dest);
            }

            Operation::Select {
                a: _,
                b: _,
                cond,
                dest,
            } => {
                push(Some(*cond));
                assigned = Some(*dest);
            }

            Operation::Call {
                dest,
                args,
                function: _,
            } => {
                assigned = *dest;
                for vreg in args {
                    push(Some(*vreg));
                }
            }
        }

        (used, assigned)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Condition {
    Equal,
    NotEqual,
    UnsignedGreaterOrEqual,
    UnsignedLess,
    UnsignedGreater,
    UnsignedLessOrEqual,
    SignedGreaterOrEqual,
    SignedLess,
    SignedGreater,
    SignedLessOrEqual,
    Negative,
    PositiveOrZero,
    Overflow,
    NoOverflow,
    Always,
    Never,
}

impl Condition {
    pub fn from_ast_op(op: CompareOp, signed: bool) -> Self {
        match (op, signed) {
            (CompareOp::Equal, _) => Self::Equal,
            (CompareOp::NotEqual, _) => Self::NotEqual,
            (CompareOp::Less, true) => Self::SignedLess,
            (CompareOp::Less, false) => Self::UnsignedLess,
            (CompareOp::LessOrEqual, true) => Self::SignedLessOrEqual,
            (CompareOp::LessOrEqual, false) => Self::UnsignedLessOrEqual,
            (CompareOp::Greater, true) => Self::SignedGreater,
            (CompareOp::Greater, false) => Self::UnsignedGreater,
            (CompareOp::GreaterOrEqual, true) => Self::SignedGreaterOrEqual,
            (CompareOp::GreaterOrEqual, false) => Self::UnsignedGreaterOrEqual,
        }
    }

    pub fn inverted(&self) -> Condition {
        use Condition::*;

        match self {
            Equal => NotEqual,
            NotEqual => Equal,
            UnsignedGreaterOrEqual => UnsignedLess,
            UnsignedLess => UnsignedGreaterOrEqual,
            UnsignedGreater => UnsignedLessOrEqual,
            UnsignedLessOrEqual => UnsignedGreater,
            SignedGreaterOrEqual => SignedLess,
            SignedLess => SignedGreaterOrEqual,
            SignedGreater => SignedLessOrEqual,
            SignedLessOrEqual => SignedGreater,
            Negative => PositiveOrZero,
            PositiveOrZero => Negative,
            Overflow => NoOverflow,
            NoOverflow => Overflow,
            Always => Never,
            Never => Always,
        }
    }
}

/// A value that can be used in an operation as a source, either an immediate operand or a
/// register.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SourceVal {
    Immediate(u64),
    VReg(VirtualReg),
    String(StrId),
    StaticMem(u64),
}

impl SourceVal {
    /// Returns the virtual register if the source value is a register.
    pub fn reg(&self) -> Option<VirtualReg> {
        match self {
            Self::VReg(vreg) => Some(*vreg),
            _ => None,
        }
    }
}

impl fmt::Display for SourceVal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceVal::Immediate(n) => write!(f, "{}", n),
            SourceVal::VReg(vreg) => write!(f, "{}", vreg),
            SourceVal::String(str_id) => write!(f, "string #{}", str_id),
            SourceVal::StaticMem(offset) => write!(f, "mem +{}", offset),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VirtualReg(pub u32);

impl fmt::Display for VirtualReg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "%{}", self.0)
    }
}

impl<'s> fmt::Display for IR<'s> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (string, id) in self.strings.iter() {
            writeln!(f, "#{} => \"{}\"", id, string)?;
        }

        if !self.strings.is_empty() {
            writeln!(f)?;
        }

        for item in self.items.iter() {
            let Item::Function {
                name,
                args,
                stack,
                stack_size: _,
                size_map,
                body,
            } = item;
            write!(f, "fn {}(", name)?;
            for reg in args.iter().take(1) {
                write!(f, "{}", reg)?;
            }

            for reg in args.iter().skip(1) {
                write!(f, ", {}", reg)?;
            }

            writeln!(f, ") size_map: {:?} {{", size_map)?;

            for block in body {
                let predecessors: Vec<_> = body
                    .iter()
                    .filter_map(|bb| {
                        if bb.successors().contains(&block.label) {
                            Some(bb.label)
                        } else {
                            None
                        }
                    })
                    .collect();

                writeln!(
                    f,
                    "{} ({})  # {}",
                    block.label,
                    block.args.iter().join(", "),
                    predecessors.iter().join(", ")
                )?;

                // let find_var = |offset: u32| -> &str {
                //     stack
                //         .iter()
                //         .find(|(_, off)| **off == offset)
                //         .map(|(var, _)| var)
                //         .unwrap()
                // };

                for op in block.ops.iter() {
                    match op {
                        Operation::Assign { src, dest } => writeln!(f, "    {} = {}", dest, src)?,
                        Operation::Store { src, stack_offset } => {
                            writeln!(f, "    store {} => stack {}", src, stack_offset)?
                        }
                        Operation::Load { stack_offset, dest } => {
                            writeln!(f, "    {} = load stack {}", dest, stack_offset)?
                        }
                        Operation::LoadOffset { base, offset, dest } => {
                            writeln!(f, "    {} = ptr({}) + {}", dest, base, offset)?
                        }
                        Operation::LoadArg { offset, dest } => {
                            writeln!(f, "    load arg fp + {} => {}", offset, dest)?
                        }
                        Operation::AddressOf { val, dest } => {
                            writeln!(f, "    {} = ref {}", dest, val)?
                        }
                        Operation::AddressOfArg { offset, dest } => {
                            writeln!(f, "    {} = ref fp + {}", dest, offset)?
                        }
                        Operation::LoadPointer { ptr, size, dest } => {
                            writeln!(f, "    {} = deref {:?} {}", dest, size, ptr)?
                        }
                        Operation::StorePointer {
                            src,
                            ptr,
                            size,
                            offset,
                        } => writeln!(f, "    ptr({}) + {} = {} ({:?})", ptr, offset, src, size)?,

                        Operation::Add { a, b, dest } => {
                            writeln!(f, "    {} = {} + {}", dest, a, b)?
                        }
                        Operation::Subtract { a, b, dest } => {
                            writeln!(f, "    {} = {} - {}", dest, a, b)?
                        }
                        Operation::Multiply { a, b, dest } => {
                            writeln!(f, "    {} = {} * {}", dest, a, b)?
                        }
                        Operation::Divide { a, b, dest } => {
                            writeln!(f, "    {} = {} / {}", dest, a, b)?
                        }
                        Operation::Modulo { a, b, dest } => {
                            writeln!(f, "    {} = {} % {}", dest, a, b)?
                        }

                        Operation::Not { val, dest } => writeln!(f, "    {} = not {}", dest, val)?,
                        Operation::Negate { val, dest } => {
                            writeln!(f, "    {} = neg {}", dest, val)?
                        }

                        Operation::Compare { a, b, cond, dest } => {
                            writeln!(f, "    {} = cmp {} {:?} {}", dest, a, cond, b)?
                        }
                        Operation::Select { a, b, cond, dest } => {
                            writeln!(f, "    {} = select {} ? {} : {}", dest, cond, a, b)?
                        }
                        Operation::Call {
                            function,
                            args,
                            dest,
                        } => {
                            if let Some(dest) = dest {
                                write!(f, "    {} = call {}(", dest, function)?
                            } else {
                                write!(f, "    call {}(", function)?
                            }

                            for arg in args.iter().take(1) {
                                write!(f, "{}", arg)?;
                            }

                            for arg in args.iter().skip(1) {
                                write!(f, ", {}", arg)?;
                            }

                            writeln!(f, ")")?
                        }
                    }
                }

                match block.terminator {
                    Terminator::Branch { label } => writeln!(f, "    goto {}", label)?,
                    Terminator::BranchCond {
                        cond,
                        if_true,
                        if_false,
                    } => writeln!(
                        f,
                        "    br cond {} [\n      true => {}\n      false => {}\n    ]",
                        cond, if_true, if_false
                    )?,
                    Terminator::Return { value } => writeln!(f, "    ret {}", value)?,
                }

                writeln!(f)?;
            }

            writeln!(f, "}}\n")?;
        }

        Ok(())
    }
}
