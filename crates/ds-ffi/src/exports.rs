//! Reading a library's exported C symbols straight off its bytes: the
//! PE export directory and the ELF64 dynamic symbol table, parsed
//! dependency-free so `ds bind` reports what a linked library offers
//! without a binary-format crate. The reader is total: a file that is
//! not a recognized shared object refuses naming why, and a malformed
//! table refuses naming the offset that ran past the image — a report
//! over untrusted library bytes never panics. The never-panic contract
//! assumes a 64-bit `usize`: the `usize::try_from` narrowing and the
//! unchecked `usize` arithmetic over untrusted u32/u64 fields hold on
//! the supported matrix alone (windows-msvc, linux x86_64/aarch64), so
//! a 32-bit port needs every expect and addition audited.

/// Why a library's exported symbols cannot enumerate.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    /// The bytes are neither a PE image nor an ELF shared object.
    #[error("the library is neither a PE image nor an ELF shared object")]
    UnknownFormat,
    /// A Mach-O library, whose export table this round does not read
    /// (the matrix carries the PE and ELF legs).
    #[error("a Mach-O library's export table is outside this round: the matrix carries PE and ELF")]
    MachO,
    /// A 32-bit ELF library: this round reads ELF64 alone.
    #[error("a 32-bit ELF library refuses: this round reads ELF64 alone")]
    Elf32,
    /// The format's export tables do not read: a length or offset that
    /// runs past the image.
    #[error("the {format} export table does not read: {detail}")]
    Malformed {
        /// The format whose table refused.
        format: &'static str,
        /// What ran past the image.
        detail: String,
    },
}

/// The library bytes' exported C symbol names, deduplicated and sorted —
/// the names a linker or a bound extern can address.
///
/// # Errors
///
/// Every [`ExportError`] variant: an unrecognized or refused format, and
/// a malformed export table naming the offset that failed.
pub fn enumerate(bytes: &[u8]) -> Result<Vec<String>, ExportError> {
    if bytes.starts_with(b"MZ") {
        return pe(bytes);
    }
    if bytes.starts_with(&[0x7f, b'E', b'L', b'F']) {
        return elf(bytes);
    }
    // Mach-O magics, both endiannesses and both classes, plus the fat
    // container: named ahead of the unknown-format refusal so the report
    // says which round is missing
    for magic in [
        [0xcf, 0xfa, 0xed, 0xfe],
        [0xce, 0xfa, 0xed, 0xfe],
        [0xfe, 0xed, 0xfa, 0xcf],
        [0xfe, 0xed, 0xfa, 0xce],
        [0xca, 0xfe, 0xba, 0xbe],
    ] {
        if bytes.starts_with(&magic) {
            return Err(ExportError::MachO);
        }
    }
    Err(ExportError::UnknownFormat)
}

/// One bounds-checked little-endian read position.
struct Reader<'a> {
    bytes: &'a [u8],
}

// the ELF constants the reader names: program-header types, dynamic
// tags, and the symbol bindings the enumeration admits
const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const DT_NULL: i64 = 0;
const DT_HASH: i64 = 4;
const DT_STRTAB: i64 = 5;
const DT_SYMTAB: i64 = 6;
const DT_STRSZ: i64 = 10;
const DT_GNU_HASH: i64 = 0x6fff_fef5;
const STB_GLOBAL: u8 = 1;
const STB_WEAK: u8 = 2;

impl Reader<'_> {
    fn u16_at(&self, at: usize) -> Result<u16, String> {
        let end = at
            .checked_add(2)
            .ok_or_else(|| "the offset overflows".to_owned())?;
        let raw = self
            .bytes
            .get(at..end)
            .ok_or_else(|| format!("a u16 at {at} runs past the image"))?;
        Ok(u16::from_le_bytes(raw.try_into().expect("two bytes")))
    }

    fn u32_at(&self, at: usize) -> Result<u32, String> {
        let end = at
            .checked_add(4)
            .ok_or_else(|| "the offset overflows".to_owned())?;
        let raw = self
            .bytes
            .get(at..end)
            .ok_or_else(|| format!("a u32 at {at} runs past the image"))?;
        Ok(u32::from_le_bytes(raw.try_into().expect("four bytes")))
    }

    fn u64_at(&self, at: usize) -> Result<u64, String> {
        let end = at
            .checked_add(8)
            .ok_or_else(|| "the offset overflows".to_owned())?;
        let raw = self
            .bytes
            .get(at..end)
            .ok_or_else(|| format!("a u64 at {at} runs past the image"))?;
        Ok(u64::from_le_bytes(raw.try_into().expect("eight bytes")))
    }

    /// The NUL-terminated byte string at `at`, bounded by `until`.
    fn cstr_at(&self, at: usize, until: usize) -> Result<&[u8], String> {
        let rest = self
            .bytes
            .get(at..until)
            .ok_or_else(|| format!("a name at {at} runs past its bound"))?;
        let end = rest
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| format!("a name at {at} carries no terminator"))?;
        Ok(&rest[..end])
    }
}

/// The PE export directory's named exports.
fn pe(bytes: &[u8]) -> Result<Vec<String>, ExportError> {
    let read = Reader { bytes };
    let malformed = |detail: String| ExportError::Malformed {
        format: "PE",
        detail,
    };
    let pe_offset = usize::try_from(read.u32_at(0x3c).map_err(malformed)?).expect("u32 fits usize");
    if bytes.get(pe_offset..pe_offset.saturating_add(4)) != Some(b"PE\0\0") {
        return Err(malformed("the PE signature is absent".to_owned()));
    }
    let coff = pe_offset + 4;
    let sections = usize::from(read.u16_at(coff + 2).map_err(malformed)?);
    let optional_size = usize::from(read.u16_at(coff + 16).map_err(malformed)?);
    let optional = coff + 20;
    let directory = match read.u16_at(optional).map_err(malformed)? {
        0x10b => optional + 96,
        0x20b => optional + 112,
        magic => {
            return Err(malformed(format!(
                "the optional header's magic {magic:#x} names neither PE32 nor PE32+"
            )))
        },
    };
    let export_rva = read.u32_at(directory).map_err(malformed)?;
    if export_rva == 0 {
        return Ok(Vec::new());
    }
    // RVA to file offset through the section table: each 40-byte header
    // carries the virtual address and the raw-data position
    let section_table = optional + optional_size;
    let rva = |rva: u32| -> Result<usize, String> {
        let rva = usize::try_from(rva).expect("u32 fits usize");
        for index in 0..sections {
            let header = section_table + index * 40;
            let virtual_address =
                usize::try_from(read.u32_at(header + 12)?).expect("u32 fits usize");
            let raw_size = usize::try_from(read.u32_at(header + 16)?).expect("u32 fits usize");
            let raw_pointer = usize::try_from(read.u32_at(header + 20)?).expect("u32 fits usize");
            if raw_size > 0 && rva >= virtual_address && rva < virtual_address + raw_size {
                return Ok(raw_pointer + (rva - virtual_address));
            }
        }
        Err(format!(
            "the rva {rva:#x} maps into no section with raw data"
        ))
    };
    let directory = rva(export_rva).map_err(malformed)?;
    let name_count = read.u32_at(directory + 24).map_err(malformed)?;
    let names = rva(read.u32_at(directory + 32).map_err(malformed)?).map_err(malformed)?;
    let mut out = Vec::new();
    for index in 0..usize::try_from(name_count).expect("a u32 count fits usize") {
        let name_rva = read.u32_at(names + index * 4).map_err(malformed)?;
        let name = rva(name_rva)
            .and_then(|at| read.cstr_at(at, bytes.len()))
            .map_err(malformed)?;
        if let Ok(text) = std::str::from_utf8(name) {
            out.push(text.to_owned());
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// The ELF64 dynamic symbol table's defined, global-or-weak symbols.
fn elf(bytes: &[u8]) -> Result<Vec<String>, ExportError> {
    let read = Reader { bytes };
    let malformed = |detail: String| ExportError::Malformed {
        format: "ELF64",
        detail,
    };
    if bytes.len() < 6 {
        return Err(malformed(
            "the header is shorter than its class and data bytes".to_owned(),
        ));
    }
    // the class/data pair: ELF64 little-endian reads, any 32-bit class
    // refuses its round, a big-endian ELF64 names its endianness, and
    // every other pair names itself
    match (bytes[4], bytes[5]) {
        (2, 1) => {},
        (1, _) => return Err(ExportError::Elf32),
        (2, 2) => {
            return Err(malformed(
                "a big-endian ELF64 library refuses: this round reads little-endian alone"
                    .to_owned(),
            ))
        },
        (class, data) => {
            return Err(malformed(format!(
                "the ELF class byte {class} and data byte {data} name no little-endian ELF64 \
                 library"
            )))
        },
    }
    let phoff = usize::try_from(read.u64_at(0x20).map_err(malformed)?).expect("u64 fits usize");
    let phentsize = usize::from(read.u16_at(0x36).map_err(malformed)?);
    let phnum = usize::from(read.u16_at(0x38).map_err(malformed)?);
    let mut loads = Vec::new();
    let mut dynamic = None;
    for index in 0..phnum {
        let header = phoff + index * phentsize;
        match read.u32_at(header).map_err(malformed)? {
            PT_LOAD => loads.push((
                read.u64_at(header + 8).map_err(malformed)?,
                read.u64_at(header + 16).map_err(malformed)?,
                read.u64_at(header + 32).map_err(malformed)?,
            )),
            PT_DYNAMIC => dynamic = Some(read.u64_at(header + 8).map_err(malformed)?),
            _ => {},
        }
    }
    let vaddr = |address: u64| -> Result<usize, String> {
        let address = usize::try_from(address).expect("u64 fits usize");
        for &(offset, vaddr, filesz) in &loads {
            let vaddr = usize::try_from(vaddr).expect("u64 fits usize");
            let filesz = usize::try_from(filesz).expect("u64 fits usize");
            let offset = usize::try_from(offset).expect("u64 fits usize");
            if address >= vaddr && address < vaddr + filesz {
                return Ok(offset + (address - vaddr));
            }
        }
        Err(format!("the address {address:#x} maps into no PT_LOAD"))
    };
    // the dynamic table: DT_STRTAB, DT_SYMTAB, DT_STRSZ, and one of the
    // hash tables that bounds the symbol count
    let dynamic = dynamic.ok_or_else(|| malformed("no PT_DYNAMIC segment".to_owned()))?;
    let mut strtab = None;
    let mut symtab = None;
    let mut strsz = 0usize;
    let mut hash = None;
    let mut gnu_hash = None;
    let mut at = usize::try_from(dynamic).expect("u64 fits usize");
    loop {
        let tag = i64::try_from(read.u64_at(at).map_err(malformed)?).expect("a tag fits i64");
        let value = read.u64_at(at + 8).map_err(malformed)?;
        match tag {
            DT_NULL => break,
            DT_STRTAB => strtab = Some(value),
            DT_SYMTAB => symtab = Some(value),
            DT_STRSZ => strsz = usize::try_from(value).expect("u64 fits usize"),
            DT_HASH => hash = Some(value),
            DT_GNU_HASH => gnu_hash = Some(value),
            _ => {},
        }
        at += 16;
    }
    let strtab =
        vaddr(strtab.ok_or_else(|| malformed("no DT_STRTAB".to_owned()))?).map_err(malformed)?;
    let strtab_end = strtab
        .checked_add(strsz)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| malformed("the string table runs past the image".to_owned()))?;
    let symtab =
        vaddr(symtab.ok_or_else(|| malformed("no DT_SYMTAB".to_owned()))?).map_err(malformed)?;
    // the symbol count: System V hash tables carry it as nchain; a GNU
    // hash table names the last hashed symbol, recovered by walking the
    // chain array until a terminal entry
    let count = if let Some(hash) = hash {
        usize::try_from(
            read.u32_at(vaddr(hash).map_err(malformed)? + 4)
                .map_err(malformed)?,
        )
        .expect("u32 fits usize")
    } else if let Some(gnu) = gnu_hash {
        let gnu = vaddr(gnu).map_err(malformed)?;
        let buckets =
            usize::try_from(read.u32_at(gnu).map_err(malformed)?).expect("u32 fits usize");
        let first = usize::try_from(read.u32_at(gnu + 4).map_err(malformed)?).expect("u32 fits");
        let bloom = usize::try_from(read.u32_at(gnu + 8).map_err(malformed)?).expect("u32 fits");
        let bucket_array = gnu + 16 + bloom * 8;
        let mut last = 0;
        for index in 0..buckets {
            last = last.max(
                usize::try_from(read.u32_at(bucket_array + index * 4).map_err(malformed)?)
                    .expect("u32 fits usize"),
            );
        }
        if last < first {
            first
        } else {
            let chains = bucket_array + buckets * 4;
            loop {
                let word = read
                    .u32_at(chains + (last - first) * 4)
                    .map_err(malformed)?;
                if word & 1 == 1 {
                    break;
                }
                last += 1;
            }
            last + 1
        }
    } else {
        return Err(malformed(
            "neither DT_HASH nor DT_GNU_HASH bounds the symbol table".to_owned(),
        ));
    };
    let mut out = Vec::new();
    for index in 0..count {
        let entry = symtab + index * 24;
        let name_offset = read.u32_at(entry).map_err(malformed)? as usize;
        let info = *bytes
            .get(entry + 4)
            .ok_or_else(|| malformed(format!("a symbol at {entry} runs past the image")))?;
        let shndx = read.u16_at(entry + 6).map_err(malformed)?;
        let value = read.u64_at(entry + 8).map_err(malformed)?;
        let bind = info >> 4;
        if shndx == 0 || value == 0 || (bind != STB_GLOBAL && bind != STB_WEAK) {
            continue;
        }
        let name = read
            .cstr_at(strtab + name_offset, strtab_end)
            .map_err(|_| malformed("the symbol name runs past the string table".to_owned()))?;
        if let Ok(text) = std::str::from_utf8(name) {
            out.push(text.to_owned());
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}
