use std::{
    ffi::{CStr, CString},
    fs::{File, Permissions},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::ExitStatus,
    str::FromStr,
};

use apple_codesign::{MachOSigner, SettingsScope, SigningSettings};
use bytemuck::bytes_of;
use mach_o::{Header, LoadCommand};

use crate::synthesize::{
    arch::{Assembler, LinkableCode, MachineCode},
    exe::{
        ExecutableError,
        mac::mach_o::{NList, NListType, SectionFlags},
    },
};

use super::{
    Executable,
    mac::mach_o::{
        DyLinkerCommand, DySymTabCommand, EntryPointCommand, HeaderFlags, LinkEditDataCommand,
        MemoryPermissions, SectionHeader, SegmentCommand, SymTabCommand,
    },
};

mod mach_o;

#[derive(Default)]
pub struct AppleExecutable {
    binary_identifier: Option<String>,
    path: Option<PathBuf>,
}

impl Executable for AppleExecutable {
    fn build<A: Assembler>(&mut self, code: LinkableCode<A>, out_path: impl AsRef<Path>) {
        // TODO: transform into readable code

        let out_path = out_path.as_ref();

        // Mach-O file:
        // Header
        // LC_SEGMENT (__PAGEZERO)
        // LC_SEGMENT (__TEXT)
        // __text section header
        // __cstring section header
        // LC_SEGMENT (__DATA)
        // __bss section header
        // LC_MAIN
        // LC_LOAD_DYLINKER
        // LC_SEGMENT_64 (__LINKEDIT)
        // LC_CODE_SIGNATURE
        // LC_DYSYMTAB
        // LC_SYMTAB
        // __text section (code)
        // __cstring section
        // symbol table (nlists + string table)
        // code signature

        let linker_path = b"/usr/lib/dyld";
        let dylinker_cmd_size = align(size_of::<DyLinkerCommand>() + linker_path.len(), 8);
        let path_len_with_padding = dylinker_cmd_size - size_of::<DyLinkerCommand>();
        let mut padded_linker_path = vec![0u8; path_len_with_padding];
        padded_linker_path[..linker_path.len()].copy_from_slice(linker_path);

        let dylinker = DyLinkerCommand {
            command: LoadCommand::LoadDyLinker,
            command_size: dylinker_cmd_size as u32,
            path_str_offset: size_of::<DyLinkerCommand>() as u32,
        };

        let text_data_offset = size_of::<Header>()
            + size_of::<SegmentCommand>() // __PAGEZERO
            + size_of::<SegmentCommand>() // __TEXT
            + size_of::<SectionHeader>() // __TEXT,__text
            + size_of::<SectionHeader>() // __TEXT,__cstring
            + size_of::<SegmentCommand>() // __DATA
            + size_of::<SectionHeader>() // __DATA,__bss
            + size_of::<EntryPointCommand>() // LcEntryPoint
            + dylinker.command_size as usize
            + size_of::<SegmentCommand>()
            + size_of::<LinkEditDataCommand>()
            + size_of::<DySymTabCommand>()
            + size_of::<SymTabCommand>();

        let code_size = code.size();
        let str_literal_size: usize = code.str_literals().iter().map(|s| s.len() + 1).sum();

        let str_literal_offset = text_data_offset + code_size;
        let text_segment_end = page_align(text_data_offset as u64 + code_size as u64);
        let bss_offset = text_segment_end;
        let code = code.link(str_literal_offset, bss_offset);

        let MachineCode {
            instructions,
            entry_point_offset,
            symbols,
            str_literals,
        } = code;

        let pagezero_segment = SegmentCommand {
            command: LoadCommand::Segment,
            command_size: size_of::<SegmentCommand>() as u32,
            segment_name: b"__PAGEZERO\0\0\0\0\0\0".to_owned(),
            vmaddr: 0x0,         // Located at 0x0 to catch null pointers
            vmsize: 0x100000000, // u32::MAX + 1 to block lower 32-bit address space
            file_offset: 0x0,
            file_size: 0x0,
            max_prot: MemoryPermissions::empty(),
            init_prot: MemoryPermissions::empty(),
            section_count: 0,
            flags: 0,
        };

        let text_segment_size =
            (size_of::<SegmentCommand>() + size_of::<SectionHeader>() * 2) as u32;
        let mut text_segment = SegmentCommand {
            command: LoadCommand::Segment,
            command_size: text_segment_size,
            segment_name: b"__TEXT\0\0\0\0\0\0\0\0\0\0".to_owned(),
            vmaddr: pagezero_segment.vmsize,
            vmsize: 0, // filled in later
            file_offset: 0x0,
            file_size: 0, // filled in later
            max_prot: MemoryPermissions::ReadExecute,
            init_prot: MemoryPermissions::ReadExecute,
            section_count: 2,
            flags: 0,
        };

        let mut text_section_header = SectionHeader {
            section_name: b"__text\0\0\0\0\0\0\0\0\0\0".to_owned(),
            segment_name: b"__TEXT\0\0\0\0\0\0\0\0\0\0".to_owned(),
            addr: 0x0, // filled in later
            size: code_size as u64,
            offset: 0x0, // filled in later
            align: 0x2,
            reloff: 0,
            nreloc: 0,
            flags: SectionFlags::Regular,
            _reserved1: 0,
            _reserved2: 0,
            _reserved3: 0,
        };

        let mut cstring_section_header = SectionHeader {
            section_name: b"__cstring\0\0\0\0\0\0\0".to_owned(),
            segment_name: b"__TEXT\0\0\0\0\0\0\0\0\0\0".to_owned(),
            addr: 0x0, // filled in later
            size: str_literal_size as u64,
            offset: 0x0, // filled in later
            align: 0x1,
            reloff: 0,
            nreloc: 0,
            flags: SectionFlags::CStringLiterals,
            _reserved1: 0,
            _reserved2: 0,
            _reserved3: 0,
        };

        text_section_header.offset = text_data_offset as u32;
        text_section_header.addr = text_segment.vmaddr;
        cstring_section_header.offset =
            text_section_header.offset + text_section_header.size as u32;
        cstring_section_header.addr = text_section_header.addr + text_section_header.size;

        text_segment.file_size = text_segment_end;
        text_segment.vmsize = text_segment_end;

        let text_seg_padding = text_segment_end as usize
            - cstring_section_header.offset as usize
            - cstring_section_header.size as usize;

        let data_segment_size = (size_of::<SegmentCommand>() + size_of::<SectionHeader>()) as u32;
        let data_segment = SegmentCommand {
            command: LoadCommand::Segment,
            command_size: data_segment_size,
            segment_name: b"__DATA\0\0\0\0\0\0\0\0\0\0".to_owned(),
            vmaddr: text_segment.vmaddr + text_segment.vmsize,
            vmsize: 0x4000,
            file_offset: 0x0,
            file_size: 0x0,
            max_prot: MemoryPermissions::ReadWrite,
            init_prot: MemoryPermissions::ReadWrite,
            section_count: 1,
            flags: 0,
        };

        let bss_section_header = SectionHeader {
            section_name: b"__bss\0\0\0\0\0\0\0\0\0\0\0".to_owned(),
            segment_name: b"__DATA\0\0\0\0\0\0\0\0\0\0".to_owned(),
            addr: data_segment.vmaddr,
            size: 64,
            offset: 0,
            align: 3,
            reloff: 0,
            nreloc: 0,
            flags: SectionFlags::ZeroFill,
            _reserved1: 0,
            _reserved2: 0,
            _reserved3: 0,
        };

        let mut entry_point = EntryPointCommand {
            command: LoadCommand::EntryPoint,
            command_size: size_of::<EntryPointCommand>() as u32,
            main_offset: entry_point_offset,
            stack_size: 0,
        };

        entry_point.main_offset += text_section_header.offset as u64;

        let mut linkedit_segment = SegmentCommand {
            command: LoadCommand::Segment,
            command_size: size_of::<SegmentCommand>() as u32,
            segment_name: b"__LINKEDIT\0\0\0\0\0\0".to_owned(),
            vmaddr: data_segment.vmaddr + data_segment.vmsize,
            vmsize: 0x4000,
            file_offset: 0, // filled in later
            file_size: 0,   // filled in later
            max_prot: MemoryPermissions::empty(),
            init_prot: MemoryPermissions::empty(),
            section_count: 0,
            flags: 0,
        };

        let mut code_sig_cmd = LinkEditDataCommand {
            command: LoadCommand::CodeSignature,
            command_size: size_of::<LinkEditDataCommand>() as u32,
            data_offset: 0, // filled in later
            data_size: 0,   // filled in later
        };

        let dysymtab = DySymTabCommand {
            command: LoadCommand::DySymTab,
            command_size: size_of::<DySymTabCommand>() as u32,
            ilocalsym: 0,
            nlocalsym: 0,
            iextdefsym: 0,
            nextdefsym: 0,
            iundefsym: 0,
            nundefsym: 0,
            tocoff: 0,
            ntoc: 0,
            modtaboff: 0,
            nmodtab: 0,
            extrefsymoff: 0,
            nextrefsyms: 0,
            indirectsymoff: 0,
            nindirectsyms: 0,
            extreloff: 0,
            nextrel: 0,
            locreloff: 0,
            nlocrel: 0,
        };

        let mut symtab = SymTabCommand {
            command: LoadCommand::SymTab,
            command_size: size_of::<SymTabCommand>() as u32,
            symoff: 0,
            nsyms: symbols.len() as u32,
            stroff: 0,
            strsize: 0,
        };

        let header = Header {
            magic: mach_o::Magic::X64,
            cpu_type: mach_o::CpuType::Arm64,
            cpu_subtype: mach_o::CpuSubtype::Arm,
            file_type: mach_o::FileType::Execute,
            load_cmd_count: 9,
            load_cmd_size: pagezero_segment.command_size
                + text_segment.command_size
                + data_segment.command_size
                + entry_point.command_size
                + dylinker.command_size
                + linkedit_segment.command_size
                + code_sig_cmd.command_size
                + dysymtab.command_size
                + symtab.command_size,
            flags: HeaderFlags::PIE | HeaderFlags::DyldLink,
            _reserved: 0,
        };

        let mut codesign = [0u8; 16];
        let superblob_len = 12u32;
        let superblob_count = 0u32;
        codesign[0..4].copy_from_slice(&mach_o::CSMAGIC_EMBEDDED_SIGNATURE.to_le_bytes());
        codesign[4..8].copy_from_slice(&superblob_len.to_le_bytes());
        codesign[8..12].copy_from_slice(&superblob_count.to_le_bytes());

        let mut str_table_size = 1;
        let (nlists, str_table): (Vec<_>, Vec<_>) = symbols
            .into_iter()
            .map(|(label, offset)| {
                let nlist = NList {
                    str_table_idx: str_table_size as u32,
                    n_type: NListType::Sect,
                    n_sect: 1,
                    n_desc: 0,
                    n_value: text_segment.vmaddr + offset,
                };

                str_table_size += label.len() + 1;

                (nlist, CString::new(label).unwrap())
            })
            .unzip();

        let nlists_size = size_of::<NList>() * nlists.len();

        linkedit_segment.file_offset = text_segment_end;
        linkedit_segment.file_size = (codesign.len() + nlists_size + str_table_size) as u64;

        symtab.symoff = linkedit_segment.file_offset as u32;
        symtab.nsyms = nlists.len() as u32;
        symtab.stroff = symtab.symoff + nlists_size as u32;
        symtab.strsize = str_table_size as u32;

        code_sig_cmd.data_offset = symtab.symoff + (nlists_size + str_table_size) as u32;
        code_sig_cmd.data_size = codesign.len() as u32;

        let mut vec: Vec<u8> = Vec::new();
        vec.extend(bytes_of(&header));
        vec.extend(bytes_of(&pagezero_segment));
        vec.extend(bytes_of(&text_segment));
        vec.extend(bytes_of(&text_section_header));
        vec.extend(bytes_of(&cstring_section_header));
        vec.extend(bytes_of(&data_segment));
        vec.extend(bytes_of(&bss_section_header));
        vec.extend(bytes_of(&entry_point));
        vec.extend(bytes_of(&dylinker));
        vec.extend(&padded_linker_path);
        vec.extend(bytes_of(&linkedit_segment));
        vec.extend(bytes_of(&code_sig_cmd));
        vec.extend(bytes_of(&dysymtab));
        vec.extend(bytes_of(&symtab));
        vec.extend(instructions);
        vec.extend(
            str_literals
                .iter()
                .flat_map(|s| CString::from_str(s).unwrap().into_bytes_with_nul()),
        );
        vec.extend(&vec![0u8; text_seg_padding]);
        vec.extend(nlists.iter().flat_map(bytes_of));
        vec.push(0); // First byte of string table must be 0, so nlists can point to empty string
        vec.extend(str_table.iter().flat_map(|s| s.to_bytes_with_nul()));
        vec.extend(&codesign);

        let mut file = File::create(out_path).unwrap();

        let signer = MachOSigner::new(&vec).unwrap();
        let mut sign_settings = SigningSettings::default();
        sign_settings.set_binary_identifier(
            SettingsScope::Main,
            self.binary_identifier
                .as_ref()
                .expect("apple executables require a binary identifier"),
        );
        signer
            .write_signed_binary(&sign_settings, &mut file)
            .unwrap();

        drop(file);

        std::fs::set_permissions(out_path, Permissions::from_mode(0o755)).unwrap();

        self.path = Some(out_path.to_owned());
    }

    fn with_binary_identifier(mut self, ident: impl AsRef<str>) -> Self {
        self.binary_identifier = Some(format!("com.{}", ident.as_ref()));
        self
    }

    fn run(&self) -> Result<ExitStatus, ExecutableError> {
        let Some(path) = self.path.as_ref() else {
            return Err(ExecutableError::NoBuildPath);
        };

        let exit_status = std::process::Command::new(path).status()?;

        Ok(exit_status)
    }
}

fn page_align(addr: u64) -> u64 {
    const PAGE_ALIGN: u64 = 0x4000;
    align(addr, PAGE_ALIGN)
}

fn align<
    N: std::ops::Add<Output = N>
        + std::ops::Sub<Output = N>
        + std::ops::Rem<Output = N>
        + PartialEq<N>
        + From<u8>
        + Copy,
>(
    num: N,
    alignment: N,
) -> N {
    let overshoot = num % alignment;
    if overshoot == N::from(0u8) {
        num
    } else {
        num + (alignment - overshoot)
    }
}
