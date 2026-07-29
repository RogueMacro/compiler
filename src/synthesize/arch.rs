use crate::ir::IR;

// mod _arm;
pub mod arm;

pub struct LinkableCode<A: Assembler>(pub(self) A);

impl<A: Assembler> LinkableCode<A> {
    pub fn size(&self) -> usize {
        self.0.code_size()
    }

    pub fn str_literals(&self) -> &[String] {
        self.0.str_literals()
    }

    pub fn link(self, str_literal_offset: usize, bss_offset: u64) -> MachineCode {
        self.0.into_machine_code(str_literal_offset, bss_offset)
    }
}

#[derive(Default)]
pub struct MachineCode {
    pub instructions: Vec<u8>,
    pub entry_point_offset: u64,
    pub symbols: Vec<(String, u64)>,
    pub str_literals: Vec<String>,
}

pub trait Assembler: Sized {
    fn assemble(ir: IR, main_fn: &str) -> LinkableCode<Self>;

    fn code_size(&self) -> usize;

    fn str_literals(&self) -> &[String];

    fn into_machine_code(self, str_literal_offset: usize, bss_offset: u64) -> MachineCode;
}
