use ux::u12;

use crate::{
    ir::ValSize,
    synthesize::arch::{
        Assembler,
        arm::{
            instr::{EitherOffset, EitherReg, ImmShift16, Inst},
            reg::Reg,
        },
    },
};

use super::ArmAssembler;

type BuiltinFn = fn(&mut ArmAssembler);

const PREFIX: &str = "std::syscall::";

pub fn assemble(asm: &mut ArmAssembler) {
    let builtins: &[(&str, BuiltinFn)] = &[("exit", exit), ("write", write), ("mmap", mmap)];

    for (name, assemble_fn) in builtins {
        let offset_in_bytes = asm.code_size();
        asm.functions
            .insert(format!("{}{}", PREFIX, name), offset_in_bytes / 4);
        asm.code
            .symbols
            .push((String::from(*name), offset_in_bytes as u64));

        assemble_fn(asm);
    }
}

fn load_arg(asm: &mut ArmAssembler, fp_offset: u32, dest: Reg) {
    asm.emit(Inst::Load {
        base: EitherReg::Phys(Reg::SP),
        offset: EitherOffset::Imm(u12::new(fp_offset as u16)),
        dest,
        size: ValSize::Doubleword,
    });
}

pub fn write(asm: &mut ArmAssembler) {
    load_arg(asm, 0, Reg::X0);
    load_arg(asm, 1, Reg::X1);
    load_arg(asm, 2, Reg::X2);
    syscall(asm, SyscallType::Write);
    asm.emit(Inst::Ret { value: Reg::X0 });
}

pub fn mmap(asm: &mut ArmAssembler) {
    load_arg(asm, 0, Reg::X0);
    load_arg(asm, 1, Reg::X1);
    load_arg(asm, 2, Reg::X2);
    load_arg(asm, 3, Reg::X3);
    load_arg(asm, 4, Reg::X4);
    load_arg(asm, 5, Reg::X5);
    syscall(asm, SyscallType::MMap);
    asm.emit(Inst::Ret { value: Reg::X0 });
}

pub fn exit(asm: &mut ArmAssembler) {
    load_arg(asm, 0, Reg::X0);
    syscall(asm, SyscallType::Exit);
}

fn syscall(asm: &mut ArmAssembler, typ: SyscallType) {
    asm.emit(Inst::Movz {
        shift: ImmShift16::L0,
        value: typ as u16,
        dest: Reg::X16,
    });

    asm.emit(Inst::Syscall)
}

#[repr(u16)]
pub enum SyscallType {
    Exit = 1,
    Write = 4,
    MUnmap = 73,
    MMap = 197,
}
