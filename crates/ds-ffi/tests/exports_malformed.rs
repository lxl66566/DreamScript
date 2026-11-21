//! Malformed export tables refuse naming the failing offset, never
//! panic: crafted PE and ELF64 images drive the reader past the magic
//! level into each refusal class, and every byte-prefix truncation of
//! the fixture cdylib completes without panicking.

use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

use ds_ffi::{ExportError, enumerate};

/// The `Malformed` refusal's detail, so a test can name the offset the
/// error reports.
fn malformed_detail(error: ExportError) -> String {
    let ExportError::Malformed { detail, .. } = error else {
        panic!("expected a malformed refusal, got {error:?}")
    };
    detail
}

/// A minimal PE32 image: one section mapping rva 0x1000..0x1200 onto
/// file 0x200..0x400, carrying an export directory at `export_rva` that
/// names one export. `export_rva` is the only free variable so the
/// malformed cases mutate exactly the field the refusal class lives at.
fn pe_image(export_rva: u32) -> Vec<u8> {
    let mut bytes = vec![0u8; 0x400];
    bytes[0..2].copy_from_slice(b"MZ");
    bytes[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes()); // e_lfanew
    bytes[0x40..0x44].copy_from_slice(b"PE\0\0");
    bytes[0x44..0x46].copy_from_slice(&0x14cu16.to_le_bytes()); // machine
    bytes[0x46..0x48].copy_from_slice(&1u16.to_le_bytes()); // section count
    bytes[0x54..0x56].copy_from_slice(&0xe0u16.to_le_bytes()); // optional size
    bytes[0x58..0x5a].copy_from_slice(&0x10bu16.to_le_bytes()); // PE32 magic
    bytes[0xb8..0xbc].copy_from_slice(&export_rva.to_le_bytes()); // export rva
    // the section header at 0x138: virtual address, raw size, raw pointer
    bytes[0x144..0x148].copy_from_slice(&0x1000u32.to_le_bytes());
    bytes[0x148..0x14c].copy_from_slice(&0x200u32.to_le_bytes());
    bytes[0x14c..0x150].copy_from_slice(&0x200u32.to_le_bytes());
    // the export directory at file 0x200: one named export
    bytes[0x218..0x21c].copy_from_slice(&1u32.to_le_bytes()); // name count
    bytes[0x220..0x224].copy_from_slice(&0x1040u32.to_le_bytes()); // name rvas
    bytes[0x240..0x244].copy_from_slice(&0x1060u32.to_le_bytes()); // name rva
    bytes[0x260..0x26a].copy_from_slice(b"ds_export\0");
    bytes
}

/// An e_lfanew the buffer cannot reach refuses at the signature lookup,
/// and a buffer too short for the e_lfanew slot itself refuses naming
/// the slot's offset.
#[test]
fn a_pe_e_lfanew_past_the_buffer_refuses() {
    let mut bytes = vec![0u8; 0x40];
    bytes[0..2].copy_from_slice(b"MZ");
    bytes[0x3c..0x40].copy_from_slice(&0x100u32.to_le_bytes());
    let error = enumerate(&bytes).expect_err("an unreachable e_lfanew refuses");
    assert_eq!(malformed_detail(error), "the PE signature is absent");

    let mut short = vec![0u8; 0x3c];
    short[0..2].copy_from_slice(b"MZ");
    let error = enumerate(&short).expect_err("a buffer short of e_lfanew refuses");
    assert_eq!(malformed_detail(error), "a u32 at 60 runs past the image");
}

/// An export RVA past every section's range refuses naming the rva; the
/// base image with the directory inside the section enumerates first, so
/// the refusal provably comes from the RVA walk and not an earlier check.
#[test]
fn a_pe_export_directory_past_the_last_section_refuses() {
    let base = pe_image(0x1000);
    assert_eq!(enumerate(&base).expect("the base image enumerates"), [
        "ds_export".to_owned()
    ]);
    // the sole section spans rva 0x1000..0x1200, so 0x2000 lies past it
    let bytes = pe_image(0x2000);
    let error = enumerate(&bytes).expect_err("an export rva past every section refuses");
    assert_eq!(
        malformed_detail(error),
        "the rva 0x2000 maps into no section with raw data"
    );
}

/// A minimal ELF64 shared object (496 bytes): one PT_LOAD mapping the
/// whole file at vaddr 0, a PT_DYNAMIC at 0x120, a string table at 0x180
/// bounded by DT_STRSZ, a two-entry symbol table at 0x1a0 whose second
/// entry is a defined global, and a GNU hash table at 0x1d0 whose single
/// bucket names symbol 1 with a terminal chain word.
fn elf64_image() -> Vec<u8> {
    let mut bytes = vec![0u8; 0x1f0];
    bytes[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
    bytes[4] = 2; // ELFCLASS64
    bytes[5] = 1; // little-endian
    bytes[0x20..0x28].copy_from_slice(&0x40u64.to_le_bytes()); // e_phoff
    bytes[0x36..0x38].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
    bytes[0x38..0x3a].copy_from_slice(&2u16.to_le_bytes()); // e_phnum
    // PT_LOAD at 0x40: the whole file at vaddr 0
    bytes[0x40..0x44].copy_from_slice(&1u32.to_le_bytes());
    bytes[0x60..0x68].copy_from_slice(&0x1f0u64.to_le_bytes()); // p_filesz
    // PT_DYNAMIC at 0x78: the dynamic table at 0x120
    bytes[0x78..0x7c].copy_from_slice(&2u32.to_le_bytes());
    bytes[0x80..0x88].copy_from_slice(&0x120u64.to_le_bytes()); // p_offset
    bytes[0x88..0x90].copy_from_slice(&0x120u64.to_le_bytes()); // p_vaddr
    // the dynamic table at 0x120
    bytes[0x120..0x128].copy_from_slice(&5u64.to_le_bytes()); // DT_STRTAB
    bytes[0x128..0x130].copy_from_slice(&0x180u64.to_le_bytes());
    bytes[0x130..0x138].copy_from_slice(&6u64.to_le_bytes()); // DT_SYMTAB
    bytes[0x138..0x140].copy_from_slice(&0x1a0u64.to_le_bytes());
    bytes[0x140..0x148].copy_from_slice(&10u64.to_le_bytes()); // DT_STRSZ
    bytes[0x148..0x150].copy_from_slice(&0x20u64.to_le_bytes());
    bytes[0x150..0x158].copy_from_slice(&0x6fff_fef5u64.to_le_bytes()); // DT_GNU_HASH
    bytes[0x158..0x160].copy_from_slice(&0x1d0u64.to_le_bytes());
    bytes[0x160..0x168].copy_from_slice(&0u64.to_le_bytes()); // DT_NULL
    // the string table at 0x180, bounded at 0x1a0
    bytes[0x180..0x18a].copy_from_slice(b"ds_export\0");
    // the symbol table at 0x1a0: entry 0 null, entry 1 a defined global
    bytes[0x1bc] = 0x11; // STB_GLOBAL | STT_FUNC
    bytes[0x1be..0x1c0].copy_from_slice(&1u16.to_le_bytes()); // st_shndx
    bytes[0x1c0..0x1c8].copy_from_slice(&0x1000u64.to_le_bytes()); // st_value
    // the GNU hash table at 0x1d0: header, one bloom word, one bucket
    bytes[0x1d0..0x1d4].copy_from_slice(&1u32.to_le_bytes()); // nbuckets
    bytes[0x1d4..0x1d8].copy_from_slice(&1u32.to_le_bytes()); // symoffset
    bytes[0x1d8..0x1dc].copy_from_slice(&1u32.to_le_bytes()); // bloom size
    bytes[0x1e0..0x1e8].copy_from_slice(&1u64.to_le_bytes()); // bloom word
    bytes[0x1e8..0x1ec].copy_from_slice(&1u32.to_le_bytes()); // bucket 0
    bytes[0x1ec..0x1f0].copy_from_slice(&1u32.to_le_bytes()); // chain 0, terminal
    bytes
}

/// A dynamic table cut mid-entry: the GNU_HASH tag reads but its value
/// runs past the image, the refusal naming the value's offset.
#[test]
fn an_elf64_truncated_dynamic_table_refuses() {
    let base = elf64_image();
    assert_eq!(enumerate(&base).expect("the base image enumerates"), [
        "ds_export".to_owned()
    ]);
    let mut bytes = base;
    bytes.truncate(0x158);
    let error = enumerate(&bytes).expect_err("a dynamic table cut mid-entry refuses");
    assert_eq!(malformed_detail(error), "a u64 at 344 runs past the image");
}

/// A PT_DYNAMIC whose `p_offset` and `p_vaddr` disagree pins which
/// field locates the table: the reader reads the segment's own
/// `p_offset` — the dynamic array's file location — so a `p_vaddr`
/// nothing maps changes no answer while a `p_offset` past the image
/// refuses at the offset it names; every `DT_*` pointer inside the
/// table stays vaddr-mapped through the PT_LOADs.
#[test]
fn a_pt_dynamic_locates_at_its_p_offset() {
    let mut bytes = elf64_image();
    bytes[0x88..0x90].copy_from_slice(&0x9000u64.to_le_bytes()); // p_vaddr
    assert_eq!(
        enumerate(&bytes).expect("the table reads at its p_offset"),
        ["ds_export".to_owned()]
    );

    let mut bytes = elf64_image();
    bytes[0x80..0x88].copy_from_slice(&0x9000u64.to_le_bytes()); // p_offset
    let error = enumerate(&bytes).expect_err("a p_offset past the image refuses");
    assert_eq!(
        malformed_detail(error),
        "a u64 at 36864 runs past the image"
    );
}

/// A GNU hash table whose bucket or bloom count runs past the image:
/// the bucket array walk and the bloom-word skip each refuse naming the
/// first offset past the end.
#[test]
fn an_elf64_gnu_hash_count_past_the_image_refuses() {
    // nbuckets far past the image: the bucket walk reads bucket 0 and
    // the chain word (both present, both naming symbol 1), then refuses
    // at the first absent bucket, which sits at the file's end (496)
    let mut bytes = elf64_image();
    bytes[0x1d0..0x1d4].copy_from_slice(&0x1_0000u32.to_le_bytes());
    let error = enumerate(&bytes).expect_err("a bucket count past the image refuses");
    assert_eq!(malformed_detail(error), "a u32 at 496 runs past the image");

    // bloom size 0x1000 pushes the bucket array to 0x81e0, past the
    // image, so the first bucket read refuses there
    let mut bytes = elf64_image();
    bytes[0x1d8..0x1dc].copy_from_slice(&0x1000u32.to_le_bytes());
    let error = enumerate(&bytes).expect_err("a bloom count past the image refuses");
    assert_eq!(
        malformed_detail(error),
        "a u32 at 33248 runs past the image"
    );
}

/// An ELF32 class byte, a big-endian ELF64 class/data pair, an unknown
/// class/data pair, and the Mach-O magics each refuse naming their
/// round or their own bytes.
#[test]
fn elf32_class_bytes_and_mach_o_magics_refuse_their_rounds() {
    let mut elf32 = vec![0x7f, b'E', b'L', b'F', 1, 1];
    elf32.resize(16, 0);
    assert!(matches!(enumerate(&elf32), Err(ExportError::Elf32)));

    // a true big-endian ELF64 names its endianness
    let mut be64 = vec![0x7f, b'E', b'L', b'F', 2, 2];
    be64.resize(16, 0);
    let error = enumerate(&be64).expect_err("a big-endian ELF64 refuses");
    assert_eq!(
        malformed_detail(error),
        "a big-endian ELF64 library refuses: this round reads little-endian alone"
    );

    // an unknown class byte with a little-endian data byte names its pair
    let mut odd = vec![0x7f, b'E', b'L', b'F', 0, 1];
    odd.resize(16, 0);
    let error = enumerate(&odd).expect_err("an unknown class byte refuses");
    assert_eq!(
        malformed_detail(error),
        "the ELF class byte 0 and data byte 1 name no little-endian ELF64 library"
    );

    // the fat container and the big-endian 64-bit magic
    for magic in [[0xfe, 0xed, 0xfa, 0xcf], [0xca, 0xfe, 0xba, 0xbe]] {
        let mut bytes = magic.to_vec();
        bytes.resize(16, 0);
        assert!(
            matches!(enumerate(&bytes), Err(ExportError::MachO)),
            "the magic {magic:02x?} names the Mach-O round"
        );
    }
}

/// The fixture cdylib's file name per platform, the loader-recognized
/// spelling rustc emits.
fn library_name() -> &'static str {
    if cfg!(windows) {
        "dsffi_fixture.dll"
    } else if cfg!(target_os = "macos") {
        "libdsffi_fixture.dylib"
    } else {
        "libdsffi_fixture.so"
    }
}

/// Compiles the fixture cdylib with the host's rustc into `dir`. Cargo
/// sets `RUSTC` for the test binary, so a build failure is a test
/// failure carrying rustc's own stderr — never a silent pass.
fn build_fixture(dir: &Path) -> PathBuf {
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fixture_cdylib.rs");
    let out = dir.join(library_name());
    let built = Command::new(rustc)
        .arg("--edition=2024")
        .arg("--crate-type=cdylib")
        .arg("--crate-name=dsffi_fixture")
        .arg(&source)
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap_or_else(|error| panic!("the fixture's rustc runs: {error}"));
    assert!(
        built.status.success(),
        "the fixture cdylib builds (rustc stderr: {})",
        String::from_utf8_lossy(&built.stderr)
    );
    out
}

/// Every byte-prefix truncation of the fixture cdylib completes without
/// panicking, and every prefix below the header region refuses: no
/// export-table walk can finish inside 64 bytes (the PE signature sits
/// past e_lfanew, the ELF tables sit past the program headers, and a
/// Mach-O magic refuses as soon as it is whole).
#[test]
fn every_fixture_truncation_refuses_without_panicking() {
    let dir = env::temp_dir().join(format!("ds-ffi-malformed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the temp dir creates");
    let dylib = build_fixture(&dir);
    let bytes = std::fs::read(&dylib).expect("the fixture reads back");
    for cut in 0..bytes.len() {
        let result = enumerate(&bytes[..cut]);
        if cut < 0x40 {
            assert!(
                result.is_err(),
                "a {cut}-byte prefix cannot carry an export table"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
