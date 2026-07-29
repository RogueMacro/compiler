#![allow(clippy::unusual_byte_groupings)]

use std::collections::HashMap;

use ux::{i5, i6, i7, i12, i19, i21, i26, u9, u12};

use crate::ir::{Condition, Label, Terminator, ValSize, VirtualReg};

use super::reg::Register;

/// An Armv8 instruction.
///
/// All Armv8 instructions are 32 bits long.
pub trait Instruction: std::fmt::Debug {
    fn encode(&self) -> u32;
}

impl Instruction for u32 {
    fn encode(&self) -> u32 {
        *self
    }
}

/// An immediate shift specifier with a 16-bit step size.
#[repr(u32)]
#[derive(Debug, Clone, Copy)]
pub enum ImmShift16 {
    L0 = 0,
    L16 = 1,
    L32 = 2,
    L48 = 3,
}

/// Input to an instruction that has variants for both immediate values and registers.
#[derive(Debug, Clone, Copy)]
pub enum Input<I> {
    Reg(Register),
    Imm(I),
}

fn cond_to_u32(cond: Condition) -> u32 {
    use Condition::*;

    match cond {
        Equal => 0b0000,
        NotEqual => 0b0001,
        UnsignedGreaterOrEqual => 0b0010,
        UnsignedLess => 0b0011,
        Negative => 0b0100,
        PositiveOrZero => 0b0101,
        Overflow => 0b0110,
        NoOverflow => 0b0111,
        UnsignedGreater => 0b1000,
        UnsignedLessOrEqual => 0b1001,
        SignedGreaterOrEqual => 0b1010,
        SignedLess => 0b1011,
        SignedGreater => 0b1100,
        SignedLessOrEqual => 0b1101,
        Always => 0b1110,
        Never => 0b1111,
    }
}

fn i32_to_u32(n: impl Into<i32>, bits: i32) -> u32 {
    let mask = !(i32::MIN >> (31 - bits));
    let n: i32 = n.into();
    (n & mask) as u32
}

// ----------------
// | INSTRUCTIONS |
// ----------------

#[derive(Debug, Clone, Copy)]
pub enum BranchOffset {
    Fixed(i32),
    Dynamic(Label),
}

impl BranchOffset {
    pub fn to_fixed(self, index: i32, label_map: &HashMap<Label, i32>) -> Self {
        match self {
            Self::Fixed(_) => self,
            Self::Dynamic(label) => Self::Fixed(
                *label_map
                    .get(&label)
                    .unwrap_or_else(|| panic!("expected label {} was not found", label))
                    - index,
            ),
        }
    }

    pub fn encode_bits(self, bits: i32) -> u32 {
        let Self::Fixed(offset) = self else {
            panic!("trying to encode non-fixed branch offset")
        };
        i32_to_u32(offset, bits)
    }
}

#[derive(Debug, Clone)]
pub enum FnOffset {
    Fixed(i32),
    Dynamic(String),
}

impl FnOffset {
    pub fn fix(&mut self, index: i32, fn_map: &HashMap<String, usize>) {
        *self = match self {
            Self::Fixed(i) => Self::Fixed(*i),
            Self::Dynamic(func) => Self::Fixed(
                *fn_map.get(func).unwrap_or_else(|| {
                    panic!("expected function {} was not found in {:?}", func, fn_map)
                }) as i32
                    - index,
            ),
        };
    }

    pub fn encode_bits(self, bits: i32) -> u32 {
        let Self::Fixed(offset) = self else {
            panic!("trying to encode non-fixed function offset")
        };

        i32_to_u32(offset, bits)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum PageAddr {
    Fixed(i21),
    String(usize),
    Bss(u64),
}

impl PageAddr {
    pub fn link(&mut self, index: i32, str_table_offset: usize, bss_offset: u64) {
        match self {
            PageAddr::Fixed(_) => (),
            PageAddr::String(rel_offset) => {
                let abs_offset = str_table_offset + *rel_offset;
                let page_addr = i21::new((abs_offset / 4096) as i32);
                *self = PageAddr::Fixed(page_addr);
            }
            PageAddr::Bss(rel_offset) => {
                let abs_offset = bss_offset + *rel_offset;
                let page_addr = i21::new((abs_offset / 4096) as i32);
                *self = PageAddr::Fixed(page_addr);
            }
        }
    }

    pub fn get(&self) -> i21 {
        match self {
            PageAddr::Fixed(addr) => *addr,
            _ => panic!("string page address was not linked before encoding"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum AddImmVal {
    Fixed(u12),
    String(usize),
    Bss(u64),
}

impl AddImmVal {
    pub fn link(&mut self, index: i32, str_table_offset: usize, bss_offset: u64) {
        match self {
            AddImmVal::Fixed(_) => (),
            AddImmVal::String(rel_offset) => {
                let abs_offset = str_table_offset + *rel_offset;
                let addr_in_page = u12::new((abs_offset % 4096) as u16);
                *self = AddImmVal::Fixed(addr_in_page);
            }
            AddImmVal::Bss(rel_offset) => {
                let abs_offset = bss_offset + *rel_offset;
                let addr_in_page = u12::new((abs_offset % 4096) as u16);
                *self = AddImmVal::Fixed(addr_in_page);
            }
        }
    }

    pub fn get(&self) -> u12 {
        match self {
            AddImmVal::Fixed(imm) => *imm,
            _ => panic!("immediate value address was not linked before encoding"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum EitherReg {
    Virt(VirtualReg),
    Phys(Register),
}

impl EitherReg {
    pub fn to_virtual(&self) -> Option<VirtualReg> {
        if let Self::Virt(vreg) = self {
            Some(*vreg)
        } else {
            None
        }
    }

    pub fn expect_phys(&self) -> Register {
        if let Self::Phys(reg) = self {
            *reg
        } else {
            panic!("register was not allocated")
        }
    }
}

#[derive(Debug, Clone)]
pub enum EitherOffset {
    Reg(EitherReg),
    Imm(u12),
}

impl EitherOffset {
    pub fn to_virtual(&self) -> Option<VirtualReg> {
        match self {
            Self::Reg(er) => er.to_virtual(),
            Self::Imm(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Inst<R> {
    /// ADD instruction.
    ///
    /// - shift: (00) LSL (01) LSR (10) ASR (11) Reserved
    /// - imm6: shift amount (0-63)
    /// - Rn: first source register
    /// - Rm: second source register
    /// - Rd: destination register
    Add {
        a: R,
        b: R,
        dest: R,
    },
    AddImm {
        a: EitherReg,
        imm: AddImmVal,
        dest: EitherReg,
    },

    /// ADRP instruction.
    ///
    /// Forms a PC-relative address to a 4KB page. The immediate value is left-shifted by 12 bits to
    /// form an address which is a multiple of 4KB.
    ///
    /// - immlo: lowest 2 bits of address
    /// - immhi: highest 19 bits of address
    /// - Rd: destination register
    Adrp {
        page_addr: PageAddr,
        dest: R,
    },

    /// B instruction.
    ///
    /// Branches unconditionally to a pc-relative offset.
    ///
    /// - imm26: offset encoded as offset/4
    Branch {
        offset: BranchOffset,
    },

    /// B instruction with condition.
    ///
    /// - imm19: pc relative offset to jump to (encoded as offset/4)
    /// - cond: condition as specified in [Condition]
    BranchCond {
        offset: BranchOffset,
        cond: Condition,
    },

    /// BL instruction.
    ///
    /// Stores pc+4 in lr and jumps to address.
    ///
    /// - imm26: pc relative offset (in instructions, not bytes) to jump to
    BranchLink {
        offset: FnOffset,
    },

    /// CBNZ instruction.
    ///
    /// Branch if register is not zero.
    ///
    /// - imm19: jump offset (encoded offset/4)
    /// - Rt: register to compare against
    BranchNotZero {
        offset: BranchOffset,
        reg: R,
    },

    /// CBZ instruction.
    ///
    /// Branch if register is zero.
    ///
    /// - imm19: jump offset (encoded offset/4)
    /// - Rt: register to compare against
    BranchZero {
        offset: BranchOffset,
        reg: R,
    },

    /// CMP instruction (alias of SUBS).
    Cmp {
        a: R,
        b: R,
    },

    /// CMP instruction (immediate) (alias of SUBS).
    CmpImm {
        val: R,
        imm: i12,
    },

    /// Conditional set (alias of CSINC).
    Cset {
        inv_cond: Condition,
        dest: R,
    },

    /// SDIV or UDIV instruction.
    ///
    /// - Rn: first source register
    /// - Rm: second source register
    /// - Rd: destination register
    Div {
        a: R,
        b: R,
        dest: R,
        signed: bool,
    },

    /// LDR instruction.
    ///
    /// Loads an 8 byte value from memory into a register.
    ///
    /// - imm12: offset from base (stored as a multiple of 8)
    /// - Rn: base pointer
    /// - Rt: destination register
    Load {
        base: EitherReg,
        offset: EitherOffset,
        dest: R,
        size: ValSize,
    },

    /// LDP instruction.
    ///
    /// Loads two registers from memory and updates the base register.
    ///
    /// - imm7: signed offset (scaled by 8 bytes)
    /// - Rn: base register (writeback)
    /// - Rt: first destination register
    /// - Rt2: second destination register
    LoadPair {
        base: Register,
        first: R,
        second: R,
        offset: i7,
    },

    /// MOV instruction.
    ///
    /// Copies the value in the source register to the destination register.
    ///
    /// - Rm: source register
    /// - Rd: destination register
    Mov {
        src: EitherReg,
        dest: EitherReg,
    },

    /// MOVK instruction.
    ///
    /// Moves a 16-bit immediate value into destination register, keeping all "non-affected" bits.
    ///
    /// - hw: shift left (0/16/32/48 encoded as 0/1/2/3)
    /// - imm16: 16-bit immediate value to (optionally shift) into destination register
    /// - Rd: destination register
    Movk {
        shift: ImmShift16,
        value: u16,
        dest: R,
    },

    /// MOVZ instruction.
    ///
    /// Moves a 16-bit immediate value into destination register, zeroing all "non-affected" bits. Can
    /// be combined with [MOVK](Movk) to move a 32 or 64-bit value.
    ///
    /// - hw: shift left (0/16/32/48 encoded as 0/1/2/3)
    /// - imm16: 16-bit immediate value to (optionally shift) into destination register
    /// - Rd: destination register
    Movz {
        shift: ImmShift16,
        value: u16,
        dest: R,
    },

    /// MUL instruction. (alias of MADD)
    ///
    /// Rd = Rn * Rm
    ///
    /// Encoding:
    /// 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
    /// 1  0  0  1  1  0  1  1  0  0  0  Rm             0  1  1  1  1  1  Rn             Rd
    ///
    /// - Rn: first value register
    /// - Rm: second value register
    /// - Rd: destination register
    Mul {
        a: R,
        b: R,
        dest: R,
    },

    /// NEG instruction. (alias of SUB)
    ///
    /// Encoding:
    /// 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
    /// 1  1  0  0  1  0  1  1  shift 0  Rm             imm6              1  1  1  1  1  Rd
    ///
    /// - Rm: value register
    /// - Rd: destination register
    Neg {
        val: R,
        dest: R,
    },

    /// Return from subroutine to offset stored in link register.
    ///
    /// - Rn: register containing jump address (always set to X30/LR)
    Ret {
        value: R,
    },

    /// STR instruction.
    ///
    /// Calculates an address from a base pointer/stack pointer and an offset, and saves a register
    /// value to that address.
    ///
    /// - Rm: offset register
    /// - Rn: base pointer
    /// - Rt: source register
    ///
    /// - imm12: offset (stored as a multiple of 8)
    /// - Rn: base pointer
    /// - Rt: source register
    Store {
        source: R,
        base: EitherReg,
        /// Multiple of 8 bytes.
        offset: EitherOffset,
        size: ValSize,
    },

    /// STP instruction.
    ///
    /// Stores two registers to memory and updates the base register.
    ///
    /// - imm7: signed offset (scaled by 8 bytes)
    /// - Rn: base register (writeback)
    /// - Rt: first source register
    /// - Rt2: second source register
    StorePair {
        base: Register,
        first: R,
        second: R,
        offset: i7,
    },

    /// SUB instruction.
    ///
    /// Subtracts immediate value from register.
    /// Rd = Rn - imm12
    ///
    /// - sh: 0: no shift, 1: shift left by 12 bits
    /// - shift: (00) LSL (01) LSR (10) ASR
    /// - imm12: immediate value
    /// - imm6: shift amount
    /// - Rn: first source register
    /// - Rm: second source register
    /// - Rd: destination register
    Sub {
        a: R,
        b: R,
        dest: R,
    },

    SubImm {
        a: EitherReg,
        imm: u12,
        dest: EitherReg,
    },

    /// SVC instruction.
    ///
    /// Supervisor call. 0x80 counts as a valid immediate value. Call number should be stored in X16.
    ///
    /// - imm16: 16-bit immediate value
    Svc {
        imm: u16,
    },

    /// Alias for [SVC](Svc).
    ///
    /// Uses (by convention) 0x80 for the svc immediate. Syscall number is stored in X16.
    Syscall,

    /// Used where a future instruction will be placed.
    /// Will panic if attempted encoded.
    Placeholder(Terminator),

    BeginFnCall {
        reserved_stack_size: u32,
    },
    EndFnCall,
}

impl Inst<VirtualReg> {
    pub fn regs_to_alloc(
        &self,
    ) -> (
        Option<VirtualReg>,
        Option<VirtualReg>,
        Option<VirtualReg>,
        Option<VirtualReg>,
    ) {
        match self {
            Inst::Add { a, b, dest } => (Some(*a), Some(*b), None, Some(*dest)),
            Inst::AddImm { a, dest, .. } => (a.to_virtual(), None, None, dest.to_virtual()),
            Inst::Sub { a, b, dest } => (Some(*a), Some(*b), None, Some(*dest)),
            Inst::SubImm { a, dest, .. } => (a.to_virtual(), None, None, dest.to_virtual()),
            Inst::Mul { a, b, dest } => (Some(*a), Some(*b), None, Some(*dest)),
            Inst::Div { a, b, dest, .. } => (Some(*a), Some(*b), None, Some(*dest)),

            Inst::Neg { val, dest } => (Some(*val), None, None, Some(*dest)),

            Inst::Cmp { a, b } => (Some(*a), Some(*b), None, None),
            Inst::CmpImm { val, imm: _ } => (Some(*val), None, None, None),
            Inst::Cset { inv_cond: _, dest } => (None, None, None, Some(*dest)),

            Inst::Adrp { dest, .. } => (None, None, None, Some(*dest)),
            Inst::Load {
                base, dest, offset, ..
            } => (base.to_virtual(), offset.to_virtual(), None, Some(*dest)),
            Inst::Mov { src, dest } => (src.to_virtual(), None, None, dest.to_virtual()),
            Inst::Movk { dest, .. } => (None, None, None, Some(*dest)),
            Inst::Movz { dest, .. } => (None, None, None, Some(*dest)),
            Inst::Store {
                source,
                base,
                offset,
                ..
            } => (Some(*source), base.to_virtual(), offset.to_virtual(), None),

            Inst::StorePair { first, second, .. } | Inst::LoadPair { first, second, .. } => {
                (Some(*first), Some(*second), None, None)
            }

            Inst::BranchNotZero { reg, .. } | Inst::BranchZero { reg, .. } => {
                (Some(*reg), None, None, None)
            }

            Inst::Branch { .. }
            | Inst::BranchCond { .. }
            | Inst::BranchLink { .. }
            | Inst::Placeholder(_)
            | Inst::BeginFnCall { .. }
            | Inst::EndFnCall
            | Inst::Svc { .. }
            | Inst::Syscall => (None, None, None, None),

            Inst::Ret { value } => (Some(*value), None, None, None),
        }
    }

    pub fn alloc_regs(
        self,
        regs: (
            Option<Register>,
            Option<Register>,
            Option<Register>,
            Option<Register>,
        ),
    ) -> Inst<Register> {
        match self {
            Inst::Add { .. } => Inst::Add {
                a: regs.0.unwrap(),
                b: regs.1.unwrap(),
                dest: regs.3.unwrap(),
            },
            Inst::AddImm { imm, a, dest } => Inst::AddImm {
                a: regs.0.map(EitherReg::Phys).unwrap_or(a),
                imm,
                dest: regs.3.map(EitherReg::Phys).unwrap_or(dest),
            },
            Inst::Sub { .. } => Inst::Sub {
                a: regs.0.unwrap(),
                b: regs.1.unwrap(),
                dest: regs.3.unwrap(),
            },
            Inst::SubImm { imm, a, dest } => Inst::SubImm {
                a: regs.0.map(EitherReg::Phys).unwrap_or(a),
                imm,
                dest: regs.3.map(EitherReg::Phys).unwrap_or(dest),
            },
            Inst::Mul { .. } => Inst::Mul {
                a: regs.0.unwrap(),
                b: regs.1.unwrap(),
                dest: regs.3.unwrap(),
            },
            Inst::Div { signed, .. } => Inst::Div {
                a: regs.0.unwrap(),
                b: regs.1.unwrap(),
                dest: regs.3.unwrap(),
                signed,
            },

            Inst::Neg { .. } => Inst::Neg {
                val: regs.0.unwrap(),
                dest: regs.3.unwrap(),
            },

            Inst::Cmp { .. } => Inst::Cmp {
                a: regs.0.unwrap(),
                b: regs.1.unwrap(),
            },
            Inst::CmpImm { imm, .. } => Inst::CmpImm {
                val: regs.0.unwrap(),
                imm,
            },
            Inst::Cset { inv_cond, .. } => Inst::Cset {
                inv_cond,
                dest: regs.3.unwrap(),
            },

            Inst::Adrp { page_addr, .. } => Inst::Adrp {
                page_addr,
                dest: regs.3.unwrap(),
            },
            Inst::Load {
                offset, base, size, ..
            } => Inst::Load {
                base: regs.0.map(EitherReg::Phys).unwrap_or(base),
                offset: regs
                    .1
                    .map(EitherReg::Phys)
                    .map(EitherOffset::Reg)
                    .unwrap_or(offset),
                dest: regs.3.unwrap(),
                size,
            },
            Inst::Mov { dest, src } => Inst::Mov {
                src: regs.0.map(EitherReg::Phys).unwrap_or(src),
                dest: regs.3.map(EitherReg::Phys).unwrap_or(dest),
            },
            Inst::Movk { shift, value, .. } => Inst::Movk {
                shift,
                value,
                dest: regs.3.unwrap(),
            },
            Inst::Movz { shift, value, .. } => Inst::Movz {
                shift,
                value,
                dest: regs.3.unwrap(),
            },
            Inst::Store {
                offset, base, size, ..
            } => Inst::Store {
                source: regs.0.unwrap(),
                base: regs.1.map(EitherReg::Phys).unwrap_or(base),
                offset: regs
                    .2
                    .map(EitherReg::Phys)
                    .map(EitherOffset::Reg)
                    .unwrap_or(offset),
                size,
            },
            Inst::StorePair { base, offset, .. } => Inst::StorePair {
                base,
                first: regs.0.unwrap(),
                second: regs.1.unwrap(),
                offset,
            },
            Inst::LoadPair { base, offset, .. } => Inst::LoadPair {
                base,
                first: regs.0.unwrap(),
                second: regs.1.unwrap(),
                offset,
            },

            Inst::BranchNotZero { offset, .. } => Inst::BranchNotZero {
                offset,
                reg: regs.0.unwrap(),
            },
            Inst::BranchZero { offset, .. } => Inst::BranchZero {
                offset,
                reg: regs.0.unwrap(),
            },

            Inst::Branch { offset } => Inst::Branch { offset },
            Inst::BranchCond { offset, cond } => Inst::BranchCond { offset, cond },
            Inst::BranchLink { offset } => Inst::BranchLink { offset },
            Inst::Placeholder(term) => Inst::Placeholder(term),
            Inst::BeginFnCall {
                reserved_stack_size,
            } => Inst::BeginFnCall {
                reserved_stack_size,
            },
            Inst::EndFnCall => Inst::EndFnCall,
            Inst::Ret { .. } => Inst::Ret {
                value: regs.0.unwrap(),
            },
            Inst::Svc { imm } => Inst::Svc { imm },
            Inst::Syscall => Inst::Syscall,
        }
    }
}

impl Inst<Register> {
    pub fn fix_labels(&mut self, index: i32, label_map: &HashMap<Label, i32>) {
        match self {
            Inst::Branch { offset }
            | Inst::BranchCond { offset, .. }
            | Inst::BranchNotZero { offset, .. }
            | Inst::BranchZero { offset, .. } => *offset = offset.to_fixed(index, label_map),
            _ => (),
        }
    }

    pub fn link(
        &mut self,
        index: i32,
        fn_map: &HashMap<String, usize>,
        str_table_offset: usize,
        bss_offset: u64,
    ) {
        match self {
            Inst::BranchLink { offset } => offset.fix(index, fn_map),

            Inst::Adrp { page_addr, .. } => page_addr.link(index, str_table_offset, bss_offset),

            Inst::AddImm { imm, .. } => imm.link(index, str_table_offset, bss_offset),

            _ => (),
        }
    }

    pub fn encode(self) -> u32 {
        match self {
            // Add Encoding:
            // 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
            // 1  0  0  0  1  0  1  1  shift 0  Rm             imm6              Rn             Rd
            Inst::Add { a, b, dest } => {
                let a = a as u32;
                let b = b as u32;
                let dest = dest as u32;

                (0b10001011 << 24) | (a << 16) | (b << 5) | dest
            }

            Inst::AddImm { a, imm, dest } => {
                let a = a.expect_phys() as u32;
                let imm: u32 = imm.get().into();
                let dest = dest.expect_phys() as u32;

                (0b1001000100 << 22) | (imm << 10) | (a << 5) | dest
            }

            // Encoding:
            // 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
            // 1  immlo 1  0  0  0  0  immhi                                                    Rd
            Inst::Adrp { page_addr, dest } => {
                let page_addr: i32 = page_addr.get().into();
                let page_addr = page_addr as u32;
                let dest = dest as u32;

                let up19 = (page_addr & 0b111111111111111111100) >> 2;
                let lo2 = page_addr & 0b11;

                (0b1_00_10000 << 24) | (lo2 << 29) | (up19 << 5) | dest
            }

            // Encoding:
            // 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
            // 0  0  0  1  0  1  imm26
            Inst::Branch { offset } => {
                let offset = offset.encode_bits(26);

                (0b000101 << 26) | offset
            }

            // Encoding:
            // 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
            // 0  1  0  1  0  1  0  0  imm19                                                    0  cond
            Inst::BranchCond { offset, cond } => {
                let offset = offset.encode_bits(19);
                let cond = cond_to_u32(cond.inverted());

                (0b01010100 << 24) | (offset << 5) | cond
            }

            // Encoding:
            // 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
            // 1  0  0  1  0  1  imm26
            Inst::BranchLink { offset } => {
                let addr = offset.encode_bits(26);

                0b100101_00000000000000000000000000 | addr
            }

            // Encoding:
            // 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
            // 1  0  1  1  0  1  0  1  imm19                                                    Rt
            Inst::BranchNotZero { offset, reg } => {
                let addr = offset.encode_bits(19);
                let reg = reg as u32;

                (0b10110101 << 24) | (addr << 5) | reg
            }

            // Encoding:
            // 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
            // 1  0  1  1  0  1  0  0  imm19                                                    Rt
            Inst::BranchZero { offset, reg } => {
                let addr = offset.encode_bits(19);
                let reg = reg as u32;

                (0b10110100 << 24) | (addr << 5) | reg
            }

            // Encoding:
            // 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
            // 1  1  1  0  1  0  1  1  shift 0  Rm             imm6              Rn             1  1  1  1  1
            Inst::Cmp { a, b } => {
                let a = a as u32;
                let b = b as u32;

                (0b11101011_00_0 << 21) | (b << 16) | (a << 5) | 0b11111
            }

            // Encoding:
            // 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
            // 1  1  1  1  0  0  0  1  0  sh imm12                               Rn             1  1  1  1  1
            Inst::CmpImm { val, imm } => {
                let val = val as u32;
                let imm = i32_to_u32(imm, 12);

                (0b111100010_0 << 22) | (imm << 10) | (val << 5) | 0b11111
            }

            // Encoding:
            // 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
            // 1  0  0  1  1  0  1  0  1  0  0  1  1  1  1  1  cond        0  1  1  1  1  1  1  Rd
            Inst::Cset { inv_cond, dest } => {
                let cond = cond_to_u32(inv_cond);
                let dest = dest as u32;

                (0b1001101010011111_0000_0111111 << 5) | (cond << 12) | dest
            }

            // Encoding (SDIV):
            // 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
            // 1  0  0  1  1  0  1  0  1  1  0  Rm             0  0  0  0  1  1  Rn             Rd
            Inst::Div { a, b, dest, signed } => {
                let a = a as u32;
                let b = b as u32;
                let dest = dest as u32;

                if signed {
                    (0b10011010110_00000_000011 << 10) | (b << 16) | (a << 5) | dest
                } else {
                    todo!()
                }
            }

            // Encoding (unsigned offset):
            // 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
            // size  1  1  1  0  0  1  0  1  imm12                               Rn             Rt
            //
            // Encoding (register):
            // 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
            // size  1  1  1  0  0  0  0  1  1  Rm             option   S  1  0  Rn             Rt
            //                                                 1  1  1  0
            Inst::Load {
                base,
                offset,
                dest,
                size,
            } => {
                let base = base.expect_phys() as u32;
                let dest = dest as u32;
                let size = size as u32;

                match offset {
                    EitherOffset::Reg(reg) => {
                        let reg = reg.expect_phys() as u32;
                        (size << 30)
                            | (0b111000011 << 21)
                            | (reg << 16)
                            | (0b111010 << 10)
                            | (base << 5)
                            | dest
                    }
                    EitherOffset::Imm(imm) => {
                        let imm = Into::<u16>::into(imm) as u32;
                        (size << 30) | (0b11100101 << 22) | (imm << 10) | (base << 5) | dest
                    }
                }
            }

            // Encoding (post-index):
            // 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
            // 1  0  1  0  1  0  0  0  1  1  imm7                 Rt2            Rn             Rt
            Inst::LoadPair {
                base,
                first,
                second,
                offset,
            } => {
                let base = base as u32;
                let first = first as u32;
                let second = second as u32;
                let imm7: i8 = offset.into();
                let imm7 = imm7 as u32 & 0b1111111;

                0b1010100011 << 22 | imm7 << 15 | (second << 10) | (base << 5) | first
            }

            // Encoding (register to register):
            // 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
            // 1  0  1  0  1  0  1  0  0  0  0  Rm             0  0  0  0  0  0  1  1  1  1  1  Rd
            //
            // Encoding (to/from SP):
            // 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
            // 1  0  0  1  0  0  0  1  0  0  0  0  0  0  0  0  0  0  0  0  0  0  Rn             Rd
            Inst::Mov { src, dest } => {
                let src = src.expect_phys();
                let src_u32 = src as u32;
                let dest = dest.expect_phys();
                let dest_u32 = dest as u32;

                if src == Register::SP || dest == Register::SP {
                    (0b1001000100000000000000 << 10) | (src_u32 << 5) | dest_u32
                } else {
                    (0b10101010000_00000_00000011111 << 5) | (src_u32 << 16) | dest_u32
                }
            }

            // Movk Encoding:
            // 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
            // 1  1  1  1  0  0  1  0  1  hw    imm16                                           Rd
            Inst::Movk { shift, value, dest } => {
                (0b111100101 << 23) | ((shift as u32) << 21) | ((value as u32) << 5) | dest as u32
            }

            // Movz Encoding:
            // 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
            // 1  1  0  1  0  0  1  0  1  hw    imm16                                           Rd
            Inst::Movz { shift, value, dest } => {
                (0b110100101 << 23) | ((shift as u32) << 21) | ((value as u32) << 5) | dest as u32
            }

            // Mul Encoding:
            // 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
            // 1  0  0  1  1  0  1  1  0  0  0  Rm             0  1  1  1  1  1  Rn             Rd
            Inst::Mul { a, b, dest } => {
                let a = a as u32;
                let b = b as u32;
                let dest = dest as u32;

                (0b10011011000_00000_011111 << 10) | (a << 16) | (b << 5) | dest
            }

            // Inst::Neg { .. } => todo!(),
            Inst::Neg { val, dest } => Inst::Sub {
                a: Register::SP,
                b: val,
                dest,
            }
            .encode(),

            // Encoding:
            // 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
            // 1  1  0  1  0  1  1  0  0  1  0  1  1  1  1  1  0  0  0  0  0  0  Rn             0  0  0  0  0
            Inst::Ret { .. } => {
                // 0b11110 -> 30 -> Link register
                // This is equivalent to: mov pc, lr
                0b1101011001011111000000_11110_00000
            }

            // Encoding (immediate, unsigned offset):
            // 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
            // size  1  1  1  0  0  1  0  0  imm12                               Rn             Rt
            Inst::Store {
                base,
                offset,
                source,
                size,
            } => {
                let base = base.expect_phys() as u32;
                let source = source as u32;
                let size = size as u32;

                match offset {
                    EitherOffset::Reg(either_reg) => todo!(),
                    EitherOffset::Imm(imm) => {
                        let imm = Into::<u16>::into(imm) as u32;
                        (size << 30) | (0b11100100 << 22) | (imm << 10) | (base << 5) | source
                    }
                }
            }

            // Encoding (pre-index):
            // 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
            // 1  0  1  0  1  0  0  1  1  0  imm7                 Rt2            Rn             Rt
            Inst::StorePair {
                base,
                first,
                second,
                offset,
            } => {
                let base = base as u32;
                let first = first as u32;
                let second = second as u32;
                let imm7: i8 = offset.into();
                let imm7 = imm7 as u32 & 0b1111111;

                (0b1010100110 << 22) | (imm7 << 15) | (second << 10) | (base << 5) | first
            }

            // Sub register Encoding:
            // 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
            // 1  1  0  0  1  0  1  1  shift 0  Rm             imm6              Rn             Rd
            Inst::Sub { a, b, dest } => {
                let a = a as u32;
                let b = b as u32;
                let dest = dest as u32;

                (0b11001011_00_0 << 21) | (b << 16) | (a << 5) | dest
            }

            // Sub immediate Encoding:
            // 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
            // 1  1  0  1  0  0  0  1  0  sh imm12                               Rn             Rd
            Inst::SubImm { a, imm, dest } => {
                let a = a.expect_phys() as u32;
                let imm: u16 = imm.into();
                let dest = dest.expect_phys() as u32;

                (0b110100010_0 << 22) | ((imm as u32) << 10) | (a << 5) | dest
            }

            // Svc Encoding:
            // 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
            // 1  1  0  1  0  1  0  0  0  0  0  imm16                                           0  0  0  0  1
            Inst::Svc { imm } => 0b11010100000_0000000000000000_00001 | ((imm as u32) << 5),

            Inst::Syscall => Inst::Svc { imm: 0x80 }.encode(),

            Inst::Placeholder(..) => {
                panic!("placeholder instruction was not replaced before encoding")
            }
            Inst::BeginFnCall { .. } => {
                panic!("begin fn call instruction was not removed before encoding")
            }
            Inst::EndFnCall => {
                panic!("end fn call instruction was not removed before encoding")
            }
        }
    }
}

/// Alias for SUB instruction.
///
/// Equivalent to SUB <Xd> XZR <Xm> (dest = zero - src)
///
/// Encoding:
/// 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
/// 1  1  0  0  1  0  1  1  shift 0  Rm             imm6              1  1  1  1  1  Rd
#[derive(Debug, Clone, Copy)]
pub struct Neg {
    src: Register,
    dest: Register,
}

impl Instruction for Neg {
    fn encode(&self) -> u32 {
        let src = self.src as u32;
        let dest = self.dest as u32;

        (0b11001011_00_0_00000_000000_11111 << 20) | (src << 16) | dest
    }
}

/// NOP instruction.
///
/// Does nothing except advance the program counter. Can be used for alignment.
///
/// Encoding:
/// 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9  8  7  6  5  4  3  2  1  0
/// 1  1  0  1  0  1  0  1  0  0  0  0  0  0  1  1  0  0  1  0  0  0  0  0  0  0  0  1  1  1  1  1
#[derive(Debug, Clone, Copy)]
pub struct Nop;

impl Instruction for Nop {
    fn encode(&self) -> u32 {
        0b11010101000000110010000000011111
    }
}
