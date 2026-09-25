//! Roc's in-memory layout of a type, as a platform's compiled host sees it.
//!
//! A hosted function takes and returns real Roc values — a `RocStr`, a `RocList`, a
//! tag union with its discriminant at a fixed offset — so calling one means knowing,
//! byte for byte, where everything goes. The rules are the compiler's, from
//! `roc-compiler/src/layout/{store,layout,field_order}.zig`, and every size here is
//! pinned in the tests against what `roc glue` generated for basic-cli
//! (`src/roc_platform_abi.rs`), which is the ground truth a host was built against.
//!
//! The DECLARED type drives the layout, never the runtime shape of a `Value`: a hosted
//! signature says `Try(List(U8), [EndOfFile, StdinErr(IOErr)])`, and that, resolved
//! through the platform's own declarations, is what gets laid out.
//!
//! 64-bit targets only. Everything pointer-sized is 8 bytes; the `x64musl` host is the
//! first target and the only one this has been checked against.

use std::collections::HashMap;

use crate::types::Type;

/// The width of a pointer on every target this supports.
const WORD: u32 = 8;

/// A field's alignment class, in the order records sort their fields by.
///
/// Not just the alignment: a pointer sorts between 4 and 8 so that the field order
/// is the same on 32-bit and 64-bit targets (`layout.zig` `SortKey`). Records lay their
/// fields out by DESCENDING class, then by name; that is why `exit_code : I32` lands
/// after two lists, and a `U64` before them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    Align1,
    Align2,
    Align4,
    Pointer,
    Align8,
    Align16,
}

impl Class {
    fn of_align(bytes: u32) -> Class {
        match bytes {
            1 => Class::Align1,
            2 => Class::Align2,
            4 => Class::Align4,
            8 => Class::Align8,
            16 => Class::Align16,
            other => unreachable!("alignment {} is not a power of two up to 16", other),
        }
    }

}

/// A scalar the host reads at its C width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scalar {
    U8, U16, U32, U64, U128,
    I8, I16, I32, I64, I128,
    F32, F64,
    /// Roc's fixed-point decimal: an `i128`.
    Dec,
    /// One byte.
    Bool,
}

impl Scalar {
    fn size(self) -> u32 {
        match self {
            Scalar::U8 | Scalar::I8 | Scalar::Bool => 1,
            Scalar::U16 | Scalar::I16 => 2,
            Scalar::U32 | Scalar::I32 | Scalar::F32 => 4,
            Scalar::U64 | Scalar::I64 | Scalar::F64 => 8,
            Scalar::U128 | Scalar::I128 | Scalar::Dec => 16,
        }
    }
}

/// One field of a laid-out record or tuple, at its offset, in MEMORY order.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    /// The field name, or the element index for a tuple.
    pub name: String,
    pub offset: u32,
    pub layout: Layout,
}

/// One tag of a laid-out union. Index in the union's `tags` is the discriminant.
#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    pub name: String,
    /// How many arguments the tag declared. A bare tag and `Ok({})` both occupy zero
    /// bytes, and only this tells a reader which one to build.
    pub arity: usize,
    /// A bare tag has a zero-sized payload; one argument is the argument itself; two
    /// or more are a tuple.
    pub payload: Layout,
}

/// What kind of thing occupies the bytes.
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    Scalar(Scalar),
    /// `bytes, capacity_or_alloc_ptr, length` — three words, and NOT the order a list
    /// uses. The high bit of the last byte marks a small string.
    Str,
    /// `bytes, length, capacity_or_alloc_ptr` — three words.
    List(Box<Layout>),
    /// A refcounted pointer. `FileReader :: Box(U64)` and its siblings are opaque
    /// handles the host owns; what it points at is never read here.
    Box,
    /// A record, padded C-style in the order `fields` gives.
    Struct(Vec<Field>),
    /// A tuple, or a tag's several arguments: the same packing, but each field's
    /// `name` is its POSITION, so a reader can put the elements back in order.
    Tuple(Vec<Field>),
    /// Payloads overlaid at offset 0, then the discriminant.
    TagUnion { tags: Vec<Variant>, disc_offset: u32, disc_size: u8 },
    /// `{}`, `()`, a union with no tags: zero bytes.
    Zst,
}

/// A type's size, alignment and shape in memory.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub size: u32,
    pub align: u32,
    /// The sort class, which for an aggregate is the greatest of its parts'.
    pub class: Class,
    pub shape: Shape,
}

/// Named types a platform declares, by name, for resolving a signature's `IOErr`.
///
/// Parameterless only: every type basic-cli's `Host.roc` names is (`NativeOsStr`,
/// `Cmd`, `PathType`, `IOErr`, …). `ponytail:` a parameterised declaration is an error
/// here; substitute its parameters when a platform needs one.
#[derive(Debug, Default, Clone)]
pub struct Declarations(pub HashMap<String, Type>);

impl Declarations {
    pub fn new(declared: impl IntoIterator<Item = (String, Type)>) -> Declarations {
        Declarations(declared.into_iter().collect())
    }
}

fn align_up(offset: u32, align: u32) -> u32 {
    (offset + align - 1) / align * align
}

fn scalar(s: Scalar) -> Layout {
    let size = s.size();
    Layout { size, align: size, class: Class::of_align(size), shape: Shape::Scalar(s) }
}

fn zst() -> Layout {
    Layout { size: 0, align: 1, class: Class::Align1, shape: Shape::Zst }
}

fn words(shape: Shape) -> Layout {
    Layout { size: 3 * WORD, align: WORD, class: Class::Pointer, shape }
}

/// Lay out `ty`, resolving declared names through `decls`.
///
/// Open records and unions (`..`) are laid out as if closed: at the host boundary the
/// type is whatever the signature wrote, and nothing can extend it after the fact.
pub fn layout_of(ty: &Type, decls: &Declarations) -> Result<Layout, String> {
    layout_at(ty, decls, 0)
}

fn layout_at(ty: &Type, decls: &Declarations, depth: u32) -> Result<Layout, String> {
    // A declaration that names itself would recurse forever; roc boxes recursive
    // types explicitly, and no hosted signature is one.
    if depth > 64 {
        return Err("type is too deeply nested to lay out (recursive declaration?)".into());
    }
    Ok(match ty {
        Type::Str => words(Shape::Str),
        Type::List(item) => words(Shape::List(Box::new(layout_at(item, decls, depth + 1)?))),
        Type::U8 => scalar(Scalar::U8),
        Type::U16 => scalar(Scalar::U16),
        Type::U32 => scalar(Scalar::U32),
        Type::U64 => scalar(Scalar::U64),
        Type::U128 => scalar(Scalar::U128),
        Type::I8 => scalar(Scalar::I8),
        Type::I16 => scalar(Scalar::I16),
        Type::I32 => scalar(Scalar::I32),
        Type::I64 => scalar(Scalar::I64),
        Type::I128 => scalar(Scalar::I128),
        Type::F32 => scalar(Scalar::F32),
        Type::F64 => scalar(Scalar::F64),
        Type::Dec => scalar(Scalar::Dec),
        Type::Bool => scalar(Scalar::Bool),
        Type::Unit => zst(),
        Type::Record { fields, .. } => {
            let mut laid = Vec::with_capacity(fields.len());
            for (name, field) in fields {
                laid.push(((*name).to_string(), layout_at(field, decls, depth + 1)?));
            }
            structure(laid, false)
        }
        Type::Tuple(items) => {
            let mut laid = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                laid.push((index.to_string(), layout_at(item, decls, depth + 1)?));
            }
            structure(laid, true)
        }
        Type::TagUnion { tags, .. } => {
            let mut variants = Vec::with_capacity(tags.len());
            for (name, args) in tags {
                let payload = match args.len() {
                    0 => zst(),
                    1 => layout_at(&args[0], decls, depth + 1)?,
                    _ => {
                        let mut laid = Vec::with_capacity(args.len());
                        for (index, arg) in args.iter().enumerate() {
                            laid.push((index.to_string(), layout_at(arg, decls, depth + 1)?));
                        }
                        structure(laid, true)
                    }
                };
                variants.push(Variant { name: (*name).to_string(), arity: args.len(), payload });
            }
            union(variants)
        }
        Type::Nominal { name, backing, .. } => {
            if *name == "Box" {
                Layout { size: WORD, align: WORD, class: Class::Pointer, shape: Shape::Box }
            } else if matches!(**backing, Type::TypeVar(u32::MAX)) {
                // The parser names a type it has not seen and drops its arguments; the
                // platform's own declaration says what it is.
                let declared = decls
                    .0
                    .get(*name)
                    .ok_or_else(|| format!("`{}` is not a type this platform declares", name))?;
                layout_at(declared, decls, depth + 1)?
            } else {
                layout_at(backing, decls, depth + 1)?
            }
        }
        Type::TypeVar(_) => return Err("an unresolved type variable has no layout".into()),
        Type::Function(..) => return Err("a function has no host layout".into()),
        Type::Optional(_) => return Err("an optional field has no host layout".into()),
        Type::Range(_) => return Err("a range has no host layout".into()),
    })
}

/// Lay out named parts as a struct: sorted by descending class, then by the order
/// given (which for a record is alphabetical, for a tuple positional), then packed
/// with C-style padding.
fn structure(mut parts: Vec<(String, Layout)>, positional: bool) -> Layout {
    if parts.is_empty() {
        return zst();
    }
    // Stable, so equal classes keep the incoming order — that is the tie-break.
    parts.sort_by(|a, b| b.1.class.cmp(&a.1.class));
    let mut offset = 0;
    let mut align = 1;
    let mut class = Class::Align1;
    let mut fields = Vec::with_capacity(parts.len());
    for (name, layout) in parts {
        offset = align_up(offset, layout.align);
        align = align.max(layout.align);
        class = class.max(layout.class);
        let size = layout.size;
        fields.push(Field { name, offset, layout });
        offset += size;
    }
    let shape = if positional { Shape::Tuple(fields) } else { Shape::Struct(fields) };
    Layout { size: align_up(offset, align), align, class, shape }
}

/// Lay out tags as a union: the widest payload, then the discriminant, rounded up.
///
/// One tag needs no discriminant and IS its payload; no tags is nothing at all.
fn union(tags: Vec<Variant>) -> Layout {
    if tags.is_empty() {
        return zst();
    }
    debug_assert!(tags.windows(2).all(|w| w[0].name < w[1].name), "tags must be sorted by name");
    let disc_size: u8 = match tags.len() {
        0..=1 => 0,
        2..=256 => 1,
        257..=65536 => 2,
        _ => 4,
    };
    let disc_align = u32::from(disc_size).max(1);
    let payload_size = tags.iter().map(|t| t.payload.size).max().unwrap_or(0);
    let payload_align = tags.iter().map(|t| t.payload.align).max().unwrap_or(1);
    let class = tags
        .iter()
        .map(|t| t.payload.class)
        .fold(Class::of_align(disc_align), Class::max);
    let disc_offset = align_up(payload_size, disc_align);
    let align = payload_align.max(disc_align);
    Layout {
        size: align_up(disc_offset + u32::from(disc_size), align),
        align,
        class,
        shape: Shape::TagUnion { tags, disc_offset, disc_size },
    }
}

#[cfg(test)]
mod tests {
    //! Sizes marked ORACLE are what `roc glue` generated for basic-cli
    //! (`src/roc_platform_abi.rs`, 64-bit), read on 2026-09-17. The rest follow from
    //! the same rules and are pinned so they cannot drift silently.
    use super::*;

    fn union_ty(tags: &[(&str, &[Type])]) -> Type {
        let mut tags: Vec<(&'static str, Vec<Type>)> =
            tags.iter().map(|(n, a)| (crate::memory::string_pool::intern(n), a.to_vec())).collect();
        tags.sort_by(|a, b| a.0.cmp(&b.0));
        Type::TagUnion { tags, open: false }
    }

    fn record(fields: &[(&str, Type)]) -> Type {
        let mut fields: Vec<(&'static str, Type)> =
            fields.iter().map(|(n, t)| (crate::memory::string_pool::intern(n), t.clone())).collect();
        fields.sort_by(|a, b| a.0.cmp(&b.0));
        Type::closed_record(fields)
    }

    fn try_ty(ok: Type, err: Type) -> Type {
        union_ty(&[("Ok", &[ok]), ("Err", &[err])])
    }

    fn list(item: Type) -> Type {
        Type::List(Box::new(item))
    }

    /// `IOErr := [AlreadyExists, …, Other(Str), …]` from basic-cli's `IOErr.roc`.
    fn io_err() -> Type {
        union_ty(&[
            ("AlreadyExists", &[]), ("BrokenPipe", &[]), ("Interrupted", &[]),
            ("IsADirectory", &[]), ("NotFound", &[]), ("NotADirectory", &[]),
            ("Other", &[Type::Str]), ("OutOfMemory", &[]), ("PermissionDenied", &[]),
            ("Unsupported", &[]),
        ])
    }

    /// `[Utf8(Str), UnixBytes(List(U8)), WindowsU16s(List(U16))]`.
    fn os_str() -> Type {
        union_ty(&[
            ("Utf8", &[Type::Str]),
            ("UnixBytes", &[list(Type::U8)]),
            ("WindowsU16s", &[list(Type::U16)]),
        ])
    }

    fn lay(ty: &Type) -> Layout {
        layout_of(ty, &Declarations::default()).expect("lays out")
    }

    fn tag_names(l: &Layout) -> Vec<&str> {
        match &l.shape {
            Shape::TagUnion { tags, .. } => tags.iter().map(|t| t.name.as_str()).collect(),
            other => panic!("not a union: {:?}", other),
        }
    }

    fn field_names(l: &Layout) -> Vec<(&str, u32)> {
        match &l.shape {
            Shape::Struct(fields) | Shape::Tuple(fields) => {
                fields.iter().map(|f| (f.name.as_str(), f.offset)).collect()
            }
            other => panic!("not a struct: {:?}", other),
        }
    }

    #[test]
    fn words_and_scalars() {
        // ORACLE: size_of::<RocStr>() == 3 * size_of::<usize>().
        assert_eq!((lay(&Type::Str).size, lay(&Type::Str).align), (24, 8));
        assert_eq!((lay(&list(Type::U8)).size, lay(&list(Type::U8)).align), (24, 8));
        let boxed = Type::Nominal { name: "Box".into(), backing: Box::new(Type::TypeVar(u32::MAX)), args: Vec::new() };
        assert_eq!((lay(&boxed).size, lay(&boxed).shape), (8, Shape::Box));
        assert_eq!(lay(&Type::Unit).size, 0);
        assert_eq!(lay(&Type::I32).size, 4);
        assert_eq!((lay(&Type::Dec).size, lay(&Type::Dec).align), (16, 16));
        assert_eq!(lay(&Type::Bool).size, 1);
    }

    #[test]
    fn os_str_is_32_bytes_with_tags_in_alphabetical_order() {
        // ORACLE: UnixBytesOrUtf8OrWindowsU16s — size 32, align 8; UnixBytes = 0,
        // Utf8 = 1, WindowsU16s = 2.
        let l = lay(&os_str());
        assert_eq!((l.size, l.align), (32, 8));
        assert_eq!(tag_names(&l), ["UnixBytes", "Utf8", "WindowsU16s"]);
        assert!(matches!(l.shape, Shape::TagUnion { disc_offset: 24, disc_size: 1, .. }));
        // A union of pointer-class payloads sorts as a pointer, not as align-8.
        assert_eq!(l.class, Class::Pointer);
    }

    #[test]
    fn io_err_is_32_bytes() {
        // ORACLE: IOErr — size 32, align 8; NotFound = 5, Other = 6.
        let l = lay(&io_err());
        assert_eq!((l.size, l.align), (32, 8));
        let names = tag_names(&l);
        assert_eq!(names[5], "NotFound");
        assert_eq!(names[6], "Other");
        assert!(matches!(l.shape, Shape::TagUnion { disc_offset: 24, disc_size: 1, .. }));
    }

    #[test]
    fn try_i32_io_err_is_40_bytes_err_first() {
        // ORACLE: HostCmdExecExitCodeResult — size 40, align 8; Err = 0, Ok = 1.
        let l = lay(&try_ty(Type::I32, io_err()));
        assert_eq!((l.size, l.align), (40, 8));
        assert_eq!(tag_names(&l), ["Err", "Ok"]);
        assert!(matches!(l.shape, Shape::TagUnion { disc_offset: 32, disc_size: 1, .. }));
    }

    #[test]
    fn cmd_record_sorts_by_class_then_name() {
        // ORACLE: AnonStructB57902ff7f66e961 (basic-cli main's Cmd) — size 160, align 8,
        // in exactly this memory order.
        let cmd = record(&[
            ("args", list(os_str())),
            ("clear_envs", Type::Bool),
            ("cwd", list(os_str())),
            ("envs", list(os_str())),
            ("manage_tree", Type::Bool),
            ("merge_stderr", Type::Bool),
            ("output_limit", Type::U64),
            ("pending_limit", Type::U64),
            ("program", os_str()),
            ("stderr_mode", Type::U8),
            ("stdin_bytes", list(Type::U8)),
            ("stdin_mode", Type::U8),
            ("stdout_mode", Type::U8),
            ("timeout_ms", Type::U64),
        ]);
        let l = lay(&cmd);
        assert_eq!((l.size, l.align), (160, 8));
        assert_eq!(
            field_names(&l),
            [
                ("output_limit", 0), ("pending_limit", 8), ("timeout_ms", 16),
                ("args", 24), ("cwd", 48), ("envs", 72), ("program", 96), ("stdin_bytes", 128),
                ("clear_envs", 152), ("manage_tree", 153), ("merge_stderr", 154),
                ("stderr_mode", 155), ("stdin_mode", 156), ("stdout_mode", 157),
            ]
        );
    }

    #[test]
    fn cmd_output_failure_puts_the_i32_last() {
        // ORACLE: AnonStruct3f89ee1e14924626 — stderr_bytes, stdout_bytes, exit_code;
        // size 56, align 8.
        let l = lay(&record(&[
            ("exit_code", Type::I32),
            ("stderr_bytes", list(Type::U8)),
            ("stdout_bytes", list(Type::U8)),
        ]));
        assert_eq!((l.size, l.align), (56, 8));
        assert_eq!(field_names(&l), [("stderr_bytes", 0), ("stdout_bytes", 24), ("exit_code", 48)]);
    }

    #[test]
    fn stdin_bytes_result() {
        // Rule-derived: `Try(List(U8), [EndOfFile, StdinErr(IOErr)])`. The error union
        // is IOErr's 32 bytes plus a discriminant → 40; the Try is that plus its own.
        let err = union_ty(&[("EndOfFile", &[]), ("StdinErr", &[io_err()])]);
        assert_eq!((lay(&err).size, lay(&err).align), (40, 8));
        let l = lay(&try_ty(list(Type::U8), err));
        assert_eq!((l.size, l.align), (48, 8));
        assert!(matches!(l.shape, Shape::TagUnion { disc_offset: 40, disc_size: 1, .. }));
    }

    #[test]
    fn main_result_is_8_bytes() {
        // Rule-derived: `Try({}, [Exit(I32), ..])`. One error tag needs no
        // discriminant, so the error IS its I32; the Try is 4 + 1, rounded to 8.
        let err = Type::TagUnion { tags: vec![("Exit".into(), vec![Type::I32])], open: true };
        assert_eq!((lay(&err).size, lay(&err).align), (4, 4));
        assert!(matches!(lay(&err).shape, Shape::TagUnion { disc_size: 0, .. }));
        let l = lay(&try_ty(Type::Unit, err));
        assert_eq!((l.size, l.align), (8, 4));
        assert!(matches!(l.shape, Shape::TagUnion { disc_offset: 4, disc_size: 1, .. }));
    }

    #[test]
    fn tuples_sort_by_class_then_position() {
        let l = lay(&Type::Tuple(vec![Type::U8, Type::I64, Type::Str]));
        assert_eq!(field_names(&l), [("1", 0), ("2", 8), ("0", 32)]);
        assert_eq!(l.size, 40);
        // Two arguments on a tag are a tuple payload.
        let l = lay(&union_ty(&[("Pair", &[Type::U8, Type::I64]), ("None", &[])]));
        assert_eq!((l.size, l.align), (24, 8));
    }

    #[test]
    fn a_declared_name_resolves_through_the_platform() {
        let named = Type::Nominal { name: "IOErr".into(), backing: Box::new(Type::TypeVar(u32::MAX)), args: Vec::new() };
        let decls = Declarations::new([("IOErr".to_string(), io_err())]);
        assert_eq!(layout_of(&named, &decls).unwrap(), lay(&io_err()));
        assert!(layout_of(&named, &Declarations::default()).unwrap_err().contains("IOErr"));
        assert!(layout_of(&Type::TypeVar(3), &decls).is_err());
    }
}
