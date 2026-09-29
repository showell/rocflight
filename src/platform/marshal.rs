//! Moving a `Value` into, and back out of, the bytes a platform's host expects.
//!
//! `layout` says where every byte goes; this puts them there. A `Str` longer than 23
//! bytes and every non-empty `List` live on the heap with Roc's refcount word in front
//! of them, allocated through whatever `Heap` the caller supplies: the host's own
//! `roc_alloc` once linked in, and an in-process fake here, which is how the whole
//! thing is tested without a host — the same bytes are written either way.
//!
//! Ownership follows the host's convention: a hosted function TAKES its arguments and
//! GIVES its result. So every argument is built fresh and never freed by rocflight,
//! and every result is read into a `Value` and then `release`d, which walks it and
//! decrements what it owned. The tests end with the heap empty.
//!
//! No `unsafe`: addresses are `usize`s the `Heap` interprets. Reading raw memory is the
//! host `Heap`'s business, in the one module allowed to.

use std::collections::HashMap;

use super::layout::{Field, Layout, Scalar, Shape, Variant};
use crate::eval::value::{str_value, Value};
use crate::memory::string_pool::intern;

/// Where refcounted data lives. Addresses are opaque to everything but the heap.
pub trait Heap {
    /// `roc_alloc`: a block of `bytes` at `align`, returning its address.
    fn alloc(&mut self, bytes: usize, align: usize) -> usize;
    /// `roc_dealloc`: free the block `alloc` returned.
    fn dealloc(&mut self, block: usize, align: usize);
    fn write(&mut self, addr: usize, bytes: &[u8]);
    fn read(&self, addr: usize, len: usize) -> Vec<u8>;
}

const WORD: usize = 8;
/// The high bit of a `Str`'s last byte: the text is inline.
const SMALL_STR: u8 = 0x80;
/// A `Str` holds at most this many bytes inline.
const SMALL_STR_MAX: usize = 3 * WORD - 1;

/// How the block around some refcounted data is shaped: the data sits `extra` bytes
/// into a block aligned to `align`, with the refcount word just before it and, when
/// the elements are themselves refcounted, their count before that
/// (`utils.zig` `allocateWithRefcount`).
fn header(elem_align: usize, elems_refcounted: bool) -> (usize, usize) {
    let align = WORD.max(elem_align);
    let required = if elems_refcounted { 2 * WORD } else { WORD };
    (required.max(elem_align), align)
}

/// Allocate `data_bytes` of refcounted data, returning the DATA address.
fn alloc_refcounted(heap: &mut dyn Heap, data_bytes: usize, elem_align: usize, count: Option<usize>) -> usize {
    let (extra, align) = header(elem_align, count.is_some());
    let block = heap.alloc(extra + data_bytes, align);
    let data = block + extra;
    heap.write(data - WORD, &1isize.to_le_bytes());
    if let Some(n) = count {
        heap.write(data - 2 * WORD, &n.to_le_bytes());
    }
    data
}

fn read_usize(heap: &dyn Heap, addr: usize) -> usize {
    usize::from_le_bytes(heap.read(addr, WORD).try_into().expect("a word"))
}

/// Drop one reference to the data at `data`. Frees it, after `on_free` has released
/// what it held, when this was the last one; leaves static data (refcount 0) alone.
fn decref(heap: &mut dyn Heap, data: usize, elem_align: usize, elems_refcounted: bool, on_free: &mut dyn FnMut(&mut dyn Heap)) {
    let rc = read_usize(heap, data - WORD) as isize;
    match rc {
        0 => {}
        1 => {
            on_free(heap);
            let (extra, align) = header(elem_align, elems_refcounted);
            heap.dealloc(data - extra, align);
        }
        n => heap.write(data - WORD, &(n - 1).to_le_bytes()),
    }
}

/// Does a value of this layout own heap data?
fn is_refcounted(l: &Layout) -> bool {
    match &l.shape {
        Shape::Str | Shape::List(_) | Shape::Box => true,
        Shape::Struct(fields) | Shape::Tuple(fields) => fields.iter().any(|f| is_refcounted(&f.layout)),
        Shape::TagUnion { tags, .. } => tags.iter().any(|t| is_refcounted(&t.payload)),
        Shape::Scalar(_) | Shape::Zst => false,
    }
}

// ---- writing -------------------------------------------------------------------

/// Write `value` as `layout` into `out`, which must be exactly `layout.size` bytes.
pub fn write_value(value: &Value, layout: &Layout, heap: &mut dyn Heap, out: &mut [u8]) -> Result<(), String> {
    debug_assert_eq!(out.len(), layout.size as usize);
    match &layout.shape {
        Shape::Scalar(s) => write_scalar(value, *s, out),
        Shape::Str => write_str(value, heap, out),
        Shape::List(item) => write_list(value, item, heap, out),
        Shape::Box => match value {
            // An opaque handle the host gave us, carried as its address.
            Value::Int(addr) => {
                out.copy_from_slice(&(*addr as u64).to_le_bytes());
                Ok(())
            }
            other => Err(format!("expected a host handle, got {}", other)),
        },
        Shape::Struct(fields) => {
            let Value::Record(given) = value else {
                return Err(format!("expected a record, got {}", value));
            };
            for field in fields {
                let (_, v) = given
                    .iter()
                    .find(|(n, _)| *n == field.name)
                    .ok_or_else(|| format!("record is missing field `{}`", field.name))?;
                write_field(v, field, heap, out)?;
            }
            Ok(())
        }
        Shape::Tuple(fields) => {
            let items: &[Value] = match value {
                Value::Tuple(items) => items,
                other => return Err(format!("expected a tuple, got {}", other)),
            };
            write_positional(items, fields, heap, out)
        }
        Shape::TagUnion { tags, disc_offset, disc_size } => {
            let Value::Tag(name, args) = value else {
                return Err(format!("expected a tag, got {}", value));
            };
            let index = tags
                .iter()
                .position(|t| t.name == *name)
                .ok_or_else(|| format!("tag `{}` is not one of the union's", name))?;
            let variant = &tags[index];
            if args.len() != variant.arity {
                return Err(format!("tag `{}` takes {} arguments, got {}", name, variant.arity, args.len()));
            }
            let payload = &mut out[..variant.payload.size as usize];
            match (variant.arity, &variant.payload.shape) {
                (0, _) => {}
                (1, _) => write_value(&args[0], &variant.payload, heap, payload)?,
                (_, Shape::Tuple(fields)) => write_positional(args, fields, heap, payload)?,
                _ => unreachable!("a tag with several arguments lays out as a tuple"),
            }
            write_disc(out, *disc_offset as usize, *disc_size, index);
            Ok(())
        }
        Shape::Zst => Ok(()),
    }
}

fn write_field(value: &Value, field: &Field, heap: &mut dyn Heap, out: &mut [u8]) -> Result<(), String> {
    let start = field.offset as usize;
    write_value(value, &field.layout, heap, &mut out[start..start + field.layout.size as usize])
}

fn write_positional(items: &[Value], fields: &[Field], heap: &mut dyn Heap, out: &mut [u8]) -> Result<(), String> {
    if items.len() != fields.len() {
        return Err(format!("expected {} elements, got {}", fields.len(), items.len()));
    }
    for field in fields {
        let index: usize = field.name.parse().expect("tuple fields are named by position");
        write_field(&items[index], field, heap, out)?;
    }
    Ok(())
}

fn write_disc(out: &mut [u8], offset: usize, size: u8, index: usize) {
    let bytes = (index as u64).to_le_bytes();
    out[offset..offset + size as usize].copy_from_slice(&bytes[..size as usize]);
}

fn write_scalar(value: &Value, scalar: Scalar, out: &mut [u8]) -> Result<(), String> {
    match (scalar, value) {
        (Scalar::Bool, Value::Bool(b)) => out[0] = *b as u8,
        (Scalar::F32, Value::Float(f)) => out.copy_from_slice(&(*f as f32).to_le_bytes()),
        (Scalar::F32, Value::F32(f)) => out.copy_from_slice(&f.to_le_bytes()),
        (Scalar::F64, Value::Float(f)) => out.copy_from_slice(&f.to_le_bytes()),
        (Scalar::F64, Value::F32(f)) => out.copy_from_slice(&f64::from(*f).to_le_bytes()),
        (Scalar::Dec, Value::Dec(d)) => out.copy_from_slice(&d.to_le_bytes()),
        // Every integer width is an i128 here; the layout says how many bytes matter,
        // and two's complement makes truncation right for both signs.
        (_, Value::Int(i)) => out.copy_from_slice(&i.to_le_bytes()[..out.len()]),
        (_, other) => return Err(format!("expected a {:?}, got {}", scalar, other)),
    }
    Ok(())
}

fn write_str(value: &Value, heap: &mut dyn Heap, out: &mut [u8]) -> Result<(), String> {
    let Value::Str(text) = value else {
        return Err(format!("expected a Str, got {}", value));
    };
    let bytes = text.as_bytes();
    out.fill(0);
    if bytes.len() <= SMALL_STR_MAX {
        out[..bytes.len()].copy_from_slice(bytes);
        out[SMALL_STR_MAX] = SMALL_STR | bytes.len() as u8;
    } else {
        let data = alloc_refcounted(heap, bytes.len(), 1, None);
        heap.write(data, bytes);
        write_words(out, [data, bytes.len() << 1, bytes.len()]);
    }
    Ok(())
}

fn write_list(value: &Value, item: &Layout, heap: &mut dyn Heap, out: &mut [u8]) -> Result<(), String> {
    let items = value.sequence().ok_or_else(|| format!("expected a List, got {}", value))?;
    if items.is_empty() {
        write_words(out, [0, 0, 0]);
        return Ok(());
    }
    let stride = item.size as usize;
    let count = is_refcounted(item).then_some(items.len());
    let data = alloc_refcounted(heap, stride * items.len(), item.align as usize, count);
    let mut scratch = vec![0u8; stride];
    for (i, v) in items.iter().enumerate() {
        write_value(v, item, heap, &mut scratch)?;
        heap.write(data + i * stride, &scratch);
    }
    write_words(out, [data, items.len(), items.len() << 1]);
    Ok(())
}

fn write_words(out: &mut [u8], words: [usize; 3]) {
    for (i, w) in words.iter().enumerate() {
        out[i * WORD..(i + 1) * WORD].copy_from_slice(&w.to_le_bytes());
    }
}

// ---- reading -------------------------------------------------------------------

/// Read a `Value` of `layout` out of `bytes`. Owns nothing: `release` afterwards.
pub fn read_value(bytes: &[u8], layout: &Layout, heap: &dyn Heap) -> Result<Value, String> {
    debug_assert_eq!(bytes.len(), layout.size as usize);
    Ok(match &layout.shape {
        Shape::Scalar(s) => read_scalar(bytes, *s),
        Shape::Str => str_value(read_str(bytes, heap)?),
        Shape::List(item) => {
            let [ptr, len, _] = read_words(bytes);
            let stride = item.size as usize;
            let mut items = Vec::with_capacity(len);
            for i in 0..len {
                items.push(read_value(&heap.read(ptr + i * stride, stride), item, heap)?);
            }
            Value::list(items)
        }
        Shape::Box => Value::Int(u64::from_le_bytes(bytes.try_into().expect("8 bytes")) as i128),
        Shape::Struct(fields) => {
            // Memory order is by alignment; a `Value`'s record is by name.
            let mut read: Vec<(&'static str, Value)> = fields
                .iter()
                .map(|f| Ok((intern(&f.name), read_field(bytes, f, heap)?)))
                .collect::<Result<_, String>>()?;
            read.sort_by(|a, b| a.0.cmp(b.0));
            Value::record(read)
        }
        Shape::Tuple(fields) => Value::tuple(read_positional(bytes, fields, heap)?),
        Shape::TagUnion { tags, disc_offset, disc_size } => {
            let index = read_disc(bytes, *disc_offset as usize, *disc_size);
            let variant = tags.get(index).ok_or_else(|| format!("discriminant {} is out of range", index))?;
            let payload = &bytes[..variant.payload.size as usize];
            let args = match (variant.arity, &variant.payload.shape) {
                (0, _) => Vec::new(),
                (1, _) => vec![read_value(payload, &variant.payload, heap)?],
                (_, Shape::Tuple(fields)) => read_positional(payload, fields, heap)?,
                _ => unreachable!("a tag with several arguments lays out as a tuple"),
            };
            Value::tag(intern(&variant.name), args)
        }
        Shape::Zst => Value::Unit,
    })
}

fn read_field(bytes: &[u8], field: &Field, heap: &dyn Heap) -> Result<Value, String> {
    let start = field.offset as usize;
    read_value(&bytes[start..start + field.layout.size as usize], &field.layout, heap)
}

fn read_positional(bytes: &[u8], fields: &[Field], heap: &dyn Heap) -> Result<Vec<Value>, String> {
    let mut items = vec![Value::Unit; fields.len()];
    for field in fields {
        let index: usize = field.name.parse().expect("tuple fields are named by position");
        items[index] = read_field(bytes, field, heap)?;
    }
    Ok(items)
}

fn read_disc(bytes: &[u8], offset: usize, size: u8) -> usize {
    let mut word = [0u8; 8];
    word[..size as usize].copy_from_slice(&bytes[offset..offset + size as usize]);
    u64::from_le_bytes(word) as usize
}

fn read_scalar(bytes: &[u8], scalar: Scalar) -> Value {
    let mut word = [0u8; 16];
    word[..bytes.len()].copy_from_slice(bytes);
    let unsigned = u128::from_le_bytes(word);
    let signed = |bits: u32| -> i128 {
        let shift = 128 - bits;
        ((unsigned as i128) << shift) >> shift
    };
    match scalar {
        Scalar::Bool => Value::Bool(bytes[0] != 0),
        Scalar::F32 => Value::F32(f32::from_le_bytes(bytes.try_into().expect("4 bytes"))),
        Scalar::F64 => Value::Float(f64::from_le_bytes(bytes.try_into().expect("8 bytes"))),
        Scalar::Dec => Value::Dec(signed(128)),
        Scalar::U8 | Scalar::U16 | Scalar::U32 | Scalar::U64 | Scalar::U128 => Value::Int(unsigned as i128),
        Scalar::I8 => Value::Int(signed(8)),
        Scalar::I16 => Value::Int(signed(16)),
        Scalar::I32 => Value::Int(signed(32)),
        Scalar::I64 => Value::Int(signed(64)),
        Scalar::I128 => Value::Int(signed(128)),
    }
}

fn read_str(bytes: &[u8], heap: &dyn Heap) -> Result<String, String> {
    let text = if bytes[SMALL_STR_MAX] & SMALL_STR != 0 {
        let len = (bytes[SMALL_STR_MAX] & !SMALL_STR) as usize;
        bytes[..len].to_vec()
    } else {
        let [ptr, _, len] = read_words(bytes);
        heap.read(ptr, len)
    };
    String::from_utf8(text).map_err(|e| format!("host returned a Str that is not UTF-8: {}", e))
}

fn read_words(bytes: &[u8]) -> [usize; 3] {
    let word = |i: usize| usize::from_le_bytes(bytes[i * WORD..(i + 1) * WORD].try_into().expect("a word"));
    [word(0), word(1), word(2)]
}

// ---- releasing -----------------------------------------------------------------

/// Give back everything a value of `layout` at `bytes` owns: one decref per string
/// and list, elements first when a list is freed.
pub fn release(bytes: &[u8], layout: &Layout, heap: &mut dyn Heap) {
    match &layout.shape {
        Shape::Str => {
            if bytes[SMALL_STR_MAX] & SMALL_STR != 0 {
                return;
            }
            let [ptr, cap_or_alloc, _] = read_words(bytes);
            // A seamless slice points at the allocation it was cut from, low bit set;
            // that is what holds the refcount.
            let data = if cap_or_alloc & 1 == 1 { cap_or_alloc & !1 } else { ptr };
            if data != 0 {
                decref(heap, data, 1, false, &mut |_| {});
            }
        }
        Shape::List(item) => {
            let [ptr, len, cap_or_alloc] = read_words(bytes);
            if ptr == 0 {
                return;
            }
            let refcounted = is_refcounted(item);
            let stride = item.size as usize;
            let (data, count) = if cap_or_alloc & 1 == 1 {
                // The original allocation records how many elements it holds.
                let data = cap_or_alloc & !1;
                let count = if refcounted { read_usize(heap, data - 2 * WORD) } else { 0 };
                (data, count)
            } else {
                (ptr, len)
            };
            decref(heap, data, item.align as usize, refcounted, &mut |heap| {
                if refcounted {
                    for i in 0..count {
                        release(&heap.read(data + i * stride, stride), item, heap);
                    }
                }
            });
        }
        // The host owns what a handle points at; nothing to give back from here.
        Shape::Box | Shape::Scalar(_) | Shape::Zst => {}
        Shape::Struct(fields) | Shape::Tuple(fields) => {
            for field in fields {
                let start = field.offset as usize;
                release(&bytes[start..start + field.layout.size as usize], &field.layout, heap);
            }
        }
        Shape::TagUnion { tags, disc_offset, disc_size } => {
            let index = read_disc(bytes, *disc_offset as usize, *disc_size);
            if let Some(Variant { payload, .. }) = tags.get(index) {
                release(&bytes[..payload.size as usize], payload, heap);
            }
        }
    }
}

/// An in-process heap: blocks in a map, addresses invented. It is what the tests
/// run against, and it counts, so a round trip can prove it gave everything back.
#[derive(Default)]
pub struct FakeHeap {
    blocks: HashMap<usize, Vec<u8>>,
    next: usize,
    pub allocs: usize,
    pub frees: usize,
}

impl FakeHeap {
    pub fn live(&self) -> usize {
        self.blocks.len()
    }

    fn locate(&self, addr: usize) -> (usize, usize) {
        self.blocks
            .iter()
            .find(|(start, block)| addr >= **start && addr < **start + block.len())
            .map(|(start, _)| (*start, addr - *start))
            .unwrap_or_else(|| panic!("address {:#x} is not inside any live block", addr))
    }
}

impl Heap for FakeHeap {
    fn alloc(&mut self, bytes: usize, align: usize) -> usize {
        // Addresses start well above zero so a null pointer never looks live, and
        // every block is followed by a gap so an overrun lands nowhere.
        self.next = (self.next.max(0x1000) + align - 1) / align * align;
        let addr = self.next;
        self.next += bytes + 64;
        self.blocks.insert(addr, vec![0; bytes]);
        self.allocs += 1;
        addr
    }

    fn dealloc(&mut self, block: usize, _align: usize) {
        assert!(self.blocks.remove(&block).is_some(), "double free or not a block start: {:#x}", block);
        self.frees += 1;
    }

    fn write(&mut self, addr: usize, bytes: &[u8]) {
        let (start, offset) = self.locate(addr);
        let block = self.blocks.get_mut(&start).expect("located");
        block[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    fn read(&self, addr: usize, len: usize) -> Vec<u8> {
        let (start, offset) = self.locate(addr);
        self.blocks[&start][offset..offset + len].to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::layout::{layout_of, Declarations};
    use crate::types::Type;

    fn lay(ty: &Type) -> Layout {
        layout_of(ty, &Declarations::default()).expect("lays out")
    }

    fn union_ty(tags: &[(&str, &[Type])]) -> Type {
        let mut tags: Vec<(&'static str, Vec<Type>)> = tags.iter().map(|(n, a)| (crate::memory::string_pool::intern(n), a.to_vec())).collect();
        tags.sort_by(|a, b| a.0.cmp(&b.0));
        Type::TagUnion { tags, open: false, row: None }
    }

    fn list(item: Type) -> Type {
        Type::List(Box::new(item))
    }

    fn io_err() -> Type {
        union_ty(&[
            ("AlreadyExists", &[]), ("BrokenPipe", &[]), ("Interrupted", &[]),
            ("IsADirectory", &[]), ("NotFound", &[]), ("NotADirectory", &[]),
            ("Other", &[Type::Str]), ("OutOfMemory", &[]), ("PermissionDenied", &[]),
            ("Unsupported", &[]),
        ])
    }

    fn os_str() -> Type {
        union_ty(&[("Utf8", &[Type::Str]), ("UnixBytes", &[list(Type::U8)]), ("WindowsU16s", &[list(Type::U16)])])
    }

    /// Write, read back, release; the value must survive and the heap must empty.
    fn round_trip(value: Value, ty: &Type) -> Vec<u8> {
        let layout = lay(ty);
        let mut heap = FakeHeap::default();
        let mut bytes = vec![0u8; layout.size as usize];
        write_value(&value, &layout, &mut heap, &mut bytes).expect("writes");
        let back = read_value(&bytes, &layout, &heap).expect("reads");
        assert_eq!(format!("{}", back), format!("{}", value), "value changed in flight");
        release(&bytes, &layout, &mut heap);
        assert_eq!(heap.live(), 0, "heap not balanced: {} allocs, {} frees", heap.allocs, heap.frees);
        bytes
    }

    #[test]
    fn small_strings_are_inline_and_touch_no_heap() {
        let bytes = round_trip(str_value("hello"), &Type::Str);
        assert_eq!(&bytes[..5], b"hello");
        assert_eq!(bytes[23], SMALL_STR | 5);
        // Exactly 23 bytes still fits; the flag byte is the length.
        let text = "abcdefghijklmnopqrstuvw";
        let bytes = round_trip(str_value(text), &Type::Str);
        assert_eq!(bytes[23], SMALL_STR | 23);
        round_trip(str_value(""), &Type::Str);
    }

    #[test]
    fn big_strings_go_on_the_heap_with_a_refcount_of_one() {
        let text = "twenty-four characters!!";
        let layout = lay(&Type::Str);
        let mut heap = FakeHeap::default();
        let mut bytes = vec![0u8; 24];
        write_value(&str_value(text), &layout, &mut heap, &mut bytes).unwrap();
        let [ptr, cap, len] = read_words(&bytes);
        assert_eq!(len, 24);
        assert_eq!(cap, 24 << 1, "capacity is stored shifted left by one");
        assert_eq!(read_usize(&heap, ptr - WORD), 1, "refcount word sits before the data");
        assert_eq!(heap.read(ptr, 24), text.as_bytes());
        release(&bytes, &layout, &mut heap);
        assert_eq!(heap.live(), 0);
        round_trip(str_value("x".repeat(1000)), &Type::Str);
    }

    #[test]
    fn a_shared_string_is_only_freed_by_its_last_owner() {
        let layout = lay(&Type::Str);
        let mut heap = FakeHeap::default();
        let mut bytes = vec![0u8; 24];
        write_value(&str_value("twenty-four characters!!"), &layout, &mut heap, &mut bytes).unwrap();
        let [ptr, ..] = read_words(&bytes);
        heap.write(ptr - WORD, &2isize.to_le_bytes());
        release(&bytes, &layout, &mut heap);
        assert_eq!(heap.live(), 1);
        assert_eq!(read_usize(&heap, ptr - WORD), 1);
        release(&bytes, &layout, &mut heap);
        assert_eq!(heap.live(), 0);
        // Static data is never freed.
        write_value(&str_value("twenty-four characters!!"), &layout, &mut heap, &mut bytes).unwrap();
        let [ptr, ..] = read_words(&bytes);
        heap.write(ptr - WORD, &0isize.to_le_bytes());
        release(&bytes, &layout, &mut heap);
        assert_eq!(heap.live(), 1);
    }

    #[test]
    fn scalars_keep_their_sign_and_width() {
        round_trip(Value::Int(-5), &Type::I8);
        round_trip(Value::Int(255), &Type::U8);
        round_trip(Value::Int(-2_000_000_000), &Type::I32);
        round_trip(Value::Int(u64::MAX as i128), &Type::U64);
        round_trip(Value::Int(i128::MIN), &Type::I128);
        round_trip(Value::Bool(true), &Type::Bool);
        round_trip(Value::Float(1.5), &Type::F64);
        round_trip(Value::Dec(147_666_666_666_666_666_666), &Type::Dec);
        round_trip(Value::Unit, &Type::Unit);
        // A U8 written from an i128 is the low byte, as in Roc.
        let bytes = round_trip(Value::Int(300), &Type::U16);
        assert_eq!(bytes, 300u16.to_le_bytes());
    }

    #[test]
    fn lists_of_scalars() {
        round_trip(Value::list(vec![Value::Int(1), Value::Int(2), Value::Int(3)]), &list(Type::U8));
        round_trip(Value::list(vec![Value::Int(-1), Value::Int(i64::MAX as i128)]), &list(Type::I64));
        let bytes = round_trip(Value::list(vec![]), &list(Type::U8));
        assert_eq!(read_words(&bytes), [0, 0, 0], "an empty list is three zero words");
    }

    #[test]
    fn lists_of_strings_record_their_element_count_and_free_their_elements() {
        let long = "a string long enough to live on the heap";
        let value = Value::list(vec![str_value("short"), str_value(long), str_value(long)]);
        let layout = lay(&list(Type::Str));
        let mut heap = FakeHeap::default();
        let mut bytes = vec![0u8; 24];
        write_value(&value, &layout, &mut heap, &mut bytes).unwrap();
        let [ptr, len, _] = read_words(&bytes);
        assert_eq!(len, 3);
        assert_eq!(heap.allocs, 3, "the list and its two big strings");
        assert_eq!(read_usize(&heap, ptr - 2 * WORD), 3, "element count before the refcount");
        release(&bytes, &layout, &mut heap);
        assert_eq!(heap.live(), 0);
        round_trip(value, &list(Type::Str));
    }

    #[test]
    fn records_and_tuples_round_trip_through_reordered_fields() {
        let ty = Type::closed_record(vec![
            ("exit_code".into(), Type::I32),
            ("stderr_bytes".into(), list(Type::U8)),
            ("stdout_bytes".into(), list(Type::U8)),
        ]);
        let value = Value::record(vec![
            ("exit_code", Value::Int(7)),
            ("stderr_bytes", Value::list(vec![])),
            ("stdout_bytes", Value::list(vec![Value::Int(72), Value::Int(105)])),
        ]);
        let bytes = round_trip(value, &ty);
        assert_eq!(&bytes[48..52], &7i32.to_le_bytes(), "the I32 sits after the two lists");
        round_trip(
            Value::tuple(vec![Value::Int(1), Value::Int(2), str_value("three")]),
            &Type::Tuple(vec![Type::U8, Type::I64, Type::Str]),
        );
    }

    #[test]
    fn tags_write_their_discriminant_after_the_payload() {
        let bytes = round_trip(Value::bare("NotFound"), &io_err());
        assert_eq!(bytes[24], 5);
        let bytes = round_trip(Value::tag("Other", [str_value("disk on fire")]), &io_err());
        assert_eq!(bytes[24], 6);
        let bytes = round_trip(Value::tag("Utf8", [str_value("/tmp")]), &os_str());
        assert_eq!(bytes[24], 1);
        round_trip(Value::tag("UnixBytes", [Value::list(vec![Value::Int(47)])]), &os_str());
        // Two arguments are a tuple payload, given back in order.
        round_trip(
            Value::tag("Pair", [Value::Int(9), str_value("nine")]),
            &union_ty(&[("Pair", &[Type::U8, Type::Str]), ("None", &[])]),
        );
    }

    #[test]
    fn try_results_as_the_host_returns_them() {
        // `Try(List(U8), [EndOfFile, StdinErr(IOErr)])`, the stdin_bytes! result.
        let err = union_ty(&[("EndOfFile", &[]), ("StdinErr", &[io_err()])]);
        let ty = union_ty(&[("Ok", &[list(Type::U8)]), ("Err", &[err])]);
        let bytes = round_trip(Value::tag("Ok", [Value::list(vec![Value::Int(10)])]), &ty);
        assert_eq!(bytes[40], 1, "Ok is 1");
        let bytes = round_trip(Value::tag("Err", [Value::bare("EndOfFile")]), &ty);
        assert_eq!((bytes[40], bytes[32]), (0, 0), "Err is 0, EndOfFile is 0");
        round_trip(
            Value::tag("Err", [Value::tag("StdinErr", [Value::tag("Other", [str_value("a long enough message to be heap allocated")])])]),
            &ty,
        );
        // `Try({}, [Exit(I32), ..])`: the single-tag error union is bare.
        let exit = Type::TagUnion { tags: vec![("Exit".into(), vec![Type::I32])], open: true, row: None };
        let ty = union_ty(&[("Ok", &[Type::Unit]), ("Err", &[exit])]);
        let bytes = round_trip(Value::tag("Err", [Value::tag("Exit", [Value::Int(3)])]), &ty);
        assert_eq!((&bytes[..4], bytes[4]), (&3i32.to_le_bytes()[..], 0));
        round_trip(Value::tag("Ok", [Value::Unit]), &ty);
    }

    #[test]
    fn a_list_of_os_strs_is_what_main_receives() {
        let args = Value::list(vec![
            Value::tag("Utf8", [str_value("program")]),
            Value::tag("Utf8", [str_value("--an-argument-long-enough-for-the-heap")]),
            Value::tag("UnixBytes", [Value::list(vec![Value::Int(255), Value::Int(0)])]),
        ]);
        round_trip(args, &list(os_str()));
    }

    #[test]
    fn a_seamless_slice_releases_its_original() {
        // The host may hand back a slice into a bigger allocation: pointer to the
        // original's data, low bit set, in the capacity word.
        let layout = lay(&Type::Str);
        let mut heap = FakeHeap::default();
        let data = alloc_refcounted(&mut heap, 64, 1, None);
        heap.write(data, b"the quick brown fox jumps over the lazy dog");
        let mut bytes = vec![0u8; 24];
        write_words(&mut bytes, [data + 4, data | 1, 5]);
        assert_eq!(format!("{}", read_value(&bytes, &layout, &heap).unwrap()), "\"quick\"");
        release(&bytes, &layout, &mut heap);
        assert_eq!(heap.live(), 0);
    }

    #[test]
    fn wrong_shapes_are_errors_not_garbage() {
        let mut heap = FakeHeap::default();
        let mut bytes = vec![0u8; 32];
        assert!(write_value(&Value::Int(1), &lay(&Type::Str), &mut heap, &mut bytes[..24]).is_err());
        assert!(write_value(&Value::bare("Nope"), &lay(&io_err()), &mut heap, &mut bytes[..32]).is_err());
        assert!(write_value(&Value::bare("Other"), &lay(&io_err()), &mut heap, &mut bytes[..32]).is_err());
        let record = lay(&Type::closed_record(vec![("x".into(), Type::I64)]));
        assert!(write_value(&Value::record(vec![]), &record, &mut heap, &mut bytes[..8]).is_err());
        assert_eq!(heap.live(), 0);
    }
}
