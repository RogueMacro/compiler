use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use colored::Colorize;
use num_derive::{FromPrimitive, ToPrimitive};
use num_traits::FromPrimitive;
use strum::EnumIter;
use ux::u12;

use crate::{
    ir::{BasicBlock, Label, Op, Operation, Terminator, ValSize, VirtualReg, lifetime::Lifetime},
    synthesize::arch::arm::{
        ArmAssembler, InstMarker,
        instr::{self, BranchOffset, EitherOffset, EitherReg, Input, Inst},
    },
};

pub type Reg = Register;

/// All general-purpose registers + stack pointer on the ARM architecture.
#[repr(u32)]
#[derive(
    EnumIter, FromPrimitive, ToPrimitive, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
pub enum Register {
    X0 = 0,   // 1st argument / return value
    X1 = 1,   // 2nd argument
    X2 = 2,   // 3rd argument
    X3 = 3,   // 4th argument
    X4 = 4,   // 5th argument
    X5 = 5,   // 6th argument
    X6 = 6,   // 7th argument
    X7 = 7,   // 8th argument
    X8 = 8,   // indirect result
    X9 = 9,   // caller-saved
    X10 = 10, // caller-saved
    X11 = 11, // caller-saved
    X12 = 12, // caller-saved
    X13 = 13, // caller-saved
    X14 = 14, // caller-saved
    X15 = 15, // caller-saved
    X16 = 16, // IP0
    X17 = 17, // IP1
    X18 = 18, // platform register
    X19 = 19, // callee-saved
    X20 = 20, // callee-saved
    X21 = 21, // callee-saved
    X22 = 22, // callee-saved
    X23 = 23, // callee-saved
    X24 = 24, // callee-saved
    X25 = 25, // callee-saved
    X26 = 26, // callee-saved
    X27 = 27, // callee-saved
    X28 = 28, // callee-saved
    FP = 29,  // frame pointer (X29)
    LR = 30,  // link register (X30)
    SP = 31,  // stack pointer (X31) (not general purpose)
}

/// Used in register allocation when mapping a virtual register to a physical register. This
/// protects a register if using the register for a value requires loading that value from stack,
/// saving the existing register value to the stack, or both.
///
/// To use this register in an operation, call the [unwrap()](Self::unwrap) method.
#[derive(Debug, Clone, Copy)]
pub enum RegisterGuard {
    Ready(Register),
    Load { load: u12, reg: Register },
    Save { save: u12, reg: Register },
    SaveAndLoad { save: u12, load: u12, reg: Register },
}

impl RegisterGuard {
    /// Returns the register that this guard protects.
    ///
    /// Do not use this register if you are not sure it doesn't overwrite a value and where the
    /// virtual register is located.
    pub fn inner_reg(&self) -> Register {
        match *self {
            RegisterGuard::Ready(reg) => reg,
            RegisterGuard::Load { load: _, reg } => reg,
            RegisterGuard::Save { save: _, reg } => reg,
            RegisterGuard::SaveAndLoad {
                save: _,
                load: _,
                reg,
            } => reg,
        }
    }

    /// Unwraps the inner register by potentially emitting a load and/or store instruction. Calling
    /// this function will ensure the value ends up in the returned register, and that the old
    /// value in the register is saved to the stack if necessary.
    pub fn unwrap(&self, asm: &mut ArmAssembler) -> Register {
        panic!("unwrap reg guard")
        // match *self {
        //     Self::Ready(reg) => reg,
        //     Self::Load { load, reg } => {
        //         asm.emit(instr::Load {
        //             base: Reg::SP,
        //             offset: load,
        //             dest: reg,
        //         });
        //         reg
        //     }
        //     Self::Save { save, reg } => {
        //         asm.emit_stack_store(save, reg);
        //         reg
        //     }
        //     Self::SaveAndLoad { save, load, reg } => {
        //         asm.emit(instr::Store {
        //             base: Reg::SP,
        //             offset: Input::Imm(save),
        //             register: reg,
        //         });
        //
        //         asm.emit(instr::Load {
        //             base: Reg::SP,
        //             offset: load,
        //             dest: reg,
        //         });
        //         reg
        //     }
        // }
    }
}

/// Location of a virtual register at any given time.
#[derive(Debug, Clone, Copy)]
pub enum Location {
    Register(Register),
    Stack(u12),
}

#[derive(Default, Debug)]
struct Stack {
    size: u16,
    map: HashMap<VirtualReg, u12>,
    free_slots: Vec<u12>,
    tmp_offset: u12,
}

impl Stack {
    pub fn alloc(&mut self, vreg: VirtualReg) -> u12 {
        let index = self.free_slots.pop().unwrap_or_else(|| {
            self.size += 1;
            u12::new(self.size - 1)
        });

        self.map.insert(vreg, index);

        index + self.tmp_offset
    }

    pub fn offset_of(&mut self, vreg: &VirtualReg) -> u12 {
        *self
            .map
            .get(vreg)
            .unwrap_or_else(|| panic!("{} is not on the stack", vreg))
            + self.tmp_offset
    }

    pub fn free(&mut self, vreg: VirtualReg) {
        let offset = self.map.remove(&vreg).unwrap();
        self.free_slots.retain(|o| *o != offset);
    }
}

use Register::*;
const CALLER_SAVED_REGS: &[Register] = &[
    X0, X1, X2, X3, X4, X5, X6, X7, X8, X9, X10, X11, X12, X13, X14, X15,
];

pub fn allocate(
    instructions: Vec<(Inst<VirtualReg>, InstMarker)>,
    stack_size: u16,
    size_map: &HashMap<VirtualReg, ValSize>,
) -> (Vec<(Inst<Register>, InstMarker)>, u16) {
    let mut final_insts = Vec::with_capacity(instructions.len());

    let mut stack = Stack::default();
    stack.size = stack_size;

    let mut regs_free: BTreeSet<Register> = BTreeSet::from_iter(CALLER_SAVED_REGS.iter().copied());
    let mut regs_in_use: BTreeSet<Register> = BTreeSet::new();

    let mut vreg_map: BTreeMap<VirtualReg, Register> = BTreeMap::new();

    let mut last_uses = HashMap::new();
    for (i, (inst, _)) in instructions.iter().enumerate() {
        let (use1, use2, use3, _) = inst.regs_to_alloc();

        let mut f = |u| {
            if let Some(u) = u {
                last_uses.insert(u, i);
            }
        };

        f(use1);
        f(use2);
        f(use3);
    }

    let mut free_regs_on_fn_call_end = Vec::new();

    for (i, (mut inst, marker)) in instructions.into_iter().enumerate() {
        match &mut inst {
            Inst::BeginFnCall {
                reserved_stack_size,
            } => {
                for (vreg, reg) in vreg_map.iter() {
                    regs_in_use.remove(reg);
                    free_regs_on_fn_call_end.push(*reg);
                    let offset: u16 = stack.alloc(*vreg).into();
                    let offset = u12::new(offset);

                    final_insts.push((
                        Inst::Store {
                            source: *reg,
                            base: EitherReg::Phys(SP),
                            offset: EitherOffset::Imm(offset),
                            size: *size_map.get(vreg).unwrap(),
                        },
                        InstMarker::None,
                    ));
                }

                stack.tmp_offset = u12::new(*reserved_stack_size as u16 / 8);

                continue;
            }
            Inst::EndFnCall => {
                stack.tmp_offset = u12::new(0);
                vreg_map.clear();
                regs_free.extend(free_regs_on_fn_call_end.drain(..));
                continue;
            }
            Inst::Load {
                base,
                offset,
                dest,
                size,
            } => {
                if matches!(base, EitherReg::Phys(Register::SP)) {
                    let EitherOffset::Imm(offset) = offset else {
                        panic!()
                    };
                    *offset = *offset + stack.tmp_offset;
                }
            }
            _ => {}
        }

        let (use1, use2, use3, dest) = inst.regs_to_alloc();

        let dest = dest.map(|vreg| {
            if let Some(reg) = vreg_map.get(&vreg) {
                return *reg;
            }

            let reg = *regs_free.iter().next().unwrap();
            regs_free.remove(&reg);
            regs_in_use.insert(reg);
            vreg_map.insert(vreg, reg);
            reg
        });

        let mut alloc_use = |vreg| {
            let reg = if let Some(reg) = vreg_map.get(&vreg) {
                *reg
            } else {
                let offset = stack.offset_of(&vreg);
                let size = *size_map.get(&vreg).unwrap();

                let reg = *regs_free.iter().next().unwrap();
                regs_free.remove(&reg);
                vreg_map.insert(vreg, reg);

                final_insts.push((
                    Inst::Load {
                        base: EitherReg::Phys(SP),
                        offset: EitherOffset::Imm(offset),
                        dest: reg,
                        size,
                    },
                    InstMarker::None,
                ));

                reg
            };

            let last_use = *last_uses.get(&vreg).unwrap();
            if i >= last_use {
                regs_in_use.remove(&reg);
                regs_free.insert(reg);
                vreg_map.remove(&vreg);
            }

            reg
        };

        let use1 = use1.map(&mut alloc_use);
        let use2 = use2.map(&mut alloc_use);
        let use3 = use3.map(&mut alloc_use);

        let inst = inst.alloc_regs((use1, use2, use3, dest));
        final_insts.push((inst, marker));
    }

    (final_insts, stack.size * 8)
}
