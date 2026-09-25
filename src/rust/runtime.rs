// The Roc builtins a roc2rust program calls, in Rust. Written by roc2rust
// (rocflight's src/rust) at the head of every program it emits.
//
// A Roc List is a value that is written in place when nothing else holds it:
// `List<T>` is an `Rc<Vec<T>>` that a writer takes whole when it holds the only
// reference and copies otherwise (`vec`), which is the same rule. An empty list
// holds nothing and allocates nothing. Integer arithmetic panics on overflow as Roc's crashes, which
// is what a debug build of this file does; the `_wrap` builtins wrap.

#![allow(dead_code, unused_variables, unused_mut, unused_parens, non_snake_case, non_camel_case_types, unreachable_patterns, unused_braces, irrefutable_let_patterns, unreachable_code)]

use std::rc::Rc;

/// Allocation counts, for measuring the translation: built only with
/// `rustc --cfg roc2rust_count_allocs`, and reported to stderr at exit.
#[cfg(roc2rust_count_allocs)]
pub mod alloc_count {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    static ALLOCS: AtomicU64 = AtomicU64::new(0);
    static REALLOCS: AtomicU64 = AtomicU64::new(0);
    static BYTES: AtomicU64 = AtomicU64::new(0);
    pub struct Counting;
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            ALLOCS.fetch_add(1, Relaxed);
            BYTES.fetch_add(layout.size() as u64, Relaxed);
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            REALLOCS.fetch_add(1, Relaxed);
            BYTES.fetch_add(new_size as u64, Relaxed);
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }
    #[global_allocator]
    static COUNTING: Counting = Counting;
    pub fn report() {
        eprintln!("allocs {} reallocs {} bytes {}", ALLOCS.load(Relaxed), REALLOCS.load(Relaxed), BYTES.load(Relaxed));
    }
}

#[derive(Clone, Default)]
pub struct List<T>(Option<Rc<Vec<T>>>);

impl<T: PartialEq> PartialEq for List<T> {
    fn eq(&self, other: &Self) -> bool {
        self.items() == other.items()
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for List<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "List({:?})", self.items())
    }
}

impl<T> List<T> {
    /// The elements, in order.
    pub fn items(&self) -> &[T] {
        self.0.as_deref().map_or(&[], |v| v.as_slice())
    }
}

impl<T: Clone> List<T> {
    pub fn of(v: Vec<T>) -> Self {
        List(if v.is_empty() { None } else { Some(Rc::new(v)) })
    }
    fn vec(self) -> Vec<T> {
        match self.0 {
            None => Vec::new(),
            Some(rc) => Rc::try_unwrap(rc).unwrap_or_else(|rc| (*rc).clone()),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.items().is_empty()
    }
    pub fn at(&self, i: usize) -> &T {
        &self.items()[i]
    }
    /// The list without its first `n` elements: a list pattern's rest.
    pub fn rest(&self, n: usize) -> List<T> {
        List::of(self.items()[n.min(self.items().len())..].to_vec())
    }
}

// ---- List ----

pub fn List__len<T: Clone>(l: List<T>) -> u64 {
    l.items().len() as u64
}
pub fn List__get<T: Clone>(l: List<T>, i: u64) -> Result<T, ()> {
    l.items().get(i as usize).cloned().ok_or(())
}
pub fn List__set<T: Clone>(l: List<T>, i: u64, x: T) -> Result<List<T>, ()> {
    let i = i as usize;
    if i >= l.items().len() {
        return Err(());
    }
    let mut v = l.vec();
    v[i] = x;
    Ok(List::of(v))
}
pub fn List__replace<T: Clone>(l: List<T>, i: u64, x: T) -> (List<T>, T) {
    let i = i as usize;
    let mut v = l.vec();
    if i >= v.len() {
        return (List::of(v), x);
    }
    let old = std::mem::replace(&mut v[i], x);
    (List::of(v), old)
}
pub fn List__insert<T: Clone>(l: List<T>, i: u64, x: T) -> Result<List<T>, ()> {
    let i = i as usize;
    if i > l.items().len() {
        return Err(());
    }
    let mut v = l.vec();
    v.insert(i, x);
    Ok(List::of(v))
}
pub fn List__append<T: Clone>(l: List<T>, x: T) -> List<T> {
    let mut v = l.vec();
    v.push(x);
    List::of(v)
}
pub fn List__concat<T: Clone>(a: List<T>, b: List<T>) -> List<T> {
    let mut v = a.vec();
    v.extend(b.items().iter().cloned());
    List::of(v)
}
pub fn List__with_capacity<T: Clone>(n: u64) -> List<T> {
    List::of(Vec::with_capacity(n as usize))
}
pub fn List__repeat<T: Clone>(x: T, n: u64) -> List<T> {
    List::of(vec![x; n as usize])
}
pub fn List__sublist<T: Clone>(l: List<T>, r: Rec_len_start) -> List<T> {
    let start = (r.start as usize).min(l.items().len());
    let end = start.saturating_add(r.len as usize).min(l.items().len());
    List::of(l.items()[start..end].to_vec())
}
pub fn List__drop_first<T: Clone>(l: List<T>, n: u64) -> List<T> {
    l.rest(n as usize)
}
pub fn List__drop_last<T: Clone>(l: List<T>, n: u64) -> List<T> {
    let keep = l.items().len().saturating_sub(n as usize);
    List::of(l.items()[..keep].to_vec())
}
pub fn List__last<T: Clone>(l: List<T>) -> Result<T, ()> {
    l.items().last().cloned().ok_or(())
}
pub fn List__map<T: Clone, U: Clone>(l: List<T>, f: &dyn Fn(T) -> U) -> List<U> {
    List::of(l.items().iter().cloned().map(|x| f(x)).collect())
}
pub fn List__fold<T: Clone, S: Clone>(l: List<T>, init: S, f: &dyn Fn(S, T) -> S) -> S {
    let mut acc = init;
    for x in l.items().iter().cloned() {
        acc = f(acc, x);
    }
    acc
}
pub fn List__starts_with<T: Clone + PartialEq>(l: List<T>, p: List<T>) -> bool {
    l.items().starts_with(p.items())
}
pub fn List__ends_with<T: Clone + PartialEq>(l: List<T>, p: List<T>) -> bool {
    l.items().ends_with(p.items())
}

pub fn List__is_empty<T: Clone>(l: List<T>) -> bool {
    l.items().is_empty()
}
pub fn List__first<T: Clone>(l: List<T>) -> Result<T, ()> {
    l.items().first().cloned().ok_or(())
}
pub fn List__prepend<T: Clone>(l: List<T>, x: T) -> List<T> {
    let mut v = Vec::with_capacity(l.items().len() + 1);
    v.push(x);
    v.extend(l.items().iter().cloned());
    List::of(v)
}
pub fn List__take_first<T: Clone>(l: List<T>, n: u64) -> List<T> {
    List::of(l.items()[..(n as usize).min(l.items().len())].to_vec())
}
pub fn List__drop_at<T: Clone>(l: List<T>, i: u64) -> List<T> {
    let i = i as usize;
    if i >= l.items().len() {
        return l;
    }
    let mut v = l.vec();
    v.remove(i);
    List::of(v)
}
pub fn List__contains<T: Clone + PartialEq>(l: List<T>, x: T) -> bool {
    l.items().contains(&x)
}
pub fn List__any<T: Clone>(l: List<T>, f: &dyn Fn(T) -> bool) -> bool {
    l.items().iter().cloned().any(|x| f(x))
}
pub fn List__all<T: Clone>(l: List<T>, f: &dyn Fn(T) -> bool) -> bool {
    l.items().iter().cloned().all(|x| f(x))
}
pub fn List__count_if<T: Clone>(l: List<T>, f: &dyn Fn(T) -> bool) -> u64 {
    l.items().iter().cloned().filter(|x| f(x.clone())).count() as u64
}
pub fn List__keep_if<T: Clone>(l: List<T>, f: &dyn Fn(T) -> bool) -> List<T> {
    List::of(l.items().iter().cloned().filter(|x| f(x.clone())).collect())
}
pub fn List__drop_if<T: Clone>(l: List<T>, f: &dyn Fn(T) -> bool) -> List<T> {
    List::of(l.items().iter().cloned().filter(|x| !f(x.clone())).collect())
}
pub fn List__find_first<T: Clone>(l: List<T>, f: &dyn Fn(T) -> bool) -> Result<T, ()> {
    l.items().iter().cloned().find(|x| f(x.clone())).ok_or(())
}
pub fn List__find_last<T: Clone>(l: List<T>, f: &dyn Fn(T) -> bool) -> Result<T, ()> {
    l.items().iter().rev().cloned().find(|x| f(x.clone())).ok_or(())
}
pub fn List__find_first_index<T: Clone>(l: List<T>, f: &dyn Fn(T) -> bool) -> Result<u64, ()> {
    l.items().iter().cloned().position(|x| f(x)).map(|i| i as u64).ok_or(())
}
pub fn List__map_with_index<T: Clone, U: Clone>(l: List<T>, f: &dyn Fn(T, u64) -> U) -> List<U> {
    List::of(l.items().iter().cloned().enumerate().map(|(i, x)| f(x, i as u64)).collect())
}
pub fn List__join<T: Clone>(l: List<List<T>>) -> List<T> {
    List::of(l.items().iter().flat_map(|x| x.items().iter().cloned()).collect())
}
pub fn List__join_map<T: Clone, U: Clone>(l: List<T>, f: &dyn Fn(T) -> List<U>) -> List<U> {
    List::of(l.items().iter().cloned().flat_map(|x| f(x).items().iter().cloned().collect::<Vec<U>>()).collect())
}
/// `List.sort_with`: a stable sort, as Roc's is, by a comparison answering the
/// program's own `[Before, Same, After]`.
pub fn List__sort_with<T: Clone, O: RocOrder>(l: List<T>, f: &dyn Fn(T, T) -> O) -> List<T> {
    let mut v = l.vec();
    v.sort_by(|a, b| f(a.clone(), b.clone()).order());
    List::of(v)
}
/// What a comparison answers, as Rust's `Ordering`; roc2rust writes the one
/// implementation, for the union `[After, Before, Same]`.
pub trait RocOrder {
    fn order(&self) -> std::cmp::Ordering;
}

/// `{ start, len }`, the argument of `List.sublist`.
#[derive(Clone, PartialEq, Debug)]
pub struct Rec_len_start {
    pub len: u64,
    pub start: u64,
}

// ---- Str ----

pub fn Str__concat(a: String, b: String) -> String {
    a + &b
}
pub fn Str__to_utf8(s: String) -> List<u8> {
    List::of(s.into_bytes())
}
pub fn Str__from_utf8_lossy(l: List<u8>) -> String {
    String::from_utf8_lossy(l.items()).into_owned()
}

pub fn Str__join_with(l: List<String>, sep: String) -> String {
    l.items().join(&sep)
}

// ---- integers ----

macro_rules! ints {
    ($($m:ident $t:ident)*) => {$( paste_int!($m, $t); )*};
}
macro_rules! paste_int {
    ($m:ident, $t:ident) => {
        pub mod $m {
            pub fn plus_wrap(a: $t, b: $t) -> $t { a.wrapping_add(b) }
            pub fn minus_wrap(a: $t, b: $t) -> $t { a.wrapping_sub(b) }
            pub fn times_wrap(a: $t, b: $t) -> $t { a.wrapping_mul(b) }
            pub fn div_trunc_by(a: $t, b: $t) -> $t { a / b }
            pub fn div_by(a: $t, b: $t) -> $t { a / b }
            pub fn rem_by(a: $t, b: $t) -> $t { a % b }
            pub fn mod_by(a: $t, b: $t) -> $t { let m = a % b; if m != 0 && ((m < 0) != (b < 0)) { m + b } else { m } }
            pub fn bitwise_and(a: $t, b: $t) -> $t { a & b }
            pub fn bitwise_or(a: $t, b: $t) -> $t { a | b }
            pub fn bitwise_xor(a: $t, b: $t) -> $t { a ^ b }
            pub fn bitwise_not(a: $t) -> $t { !a }
            pub fn min(a: $t, b: $t) -> $t { a.min(b) }
            pub fn max(a: $t, b: $t) -> $t { a.max(b) }
            pub fn shl_wrap(a: $t, n: impl Into<i128>) -> $t { a.wrapping_shl(n.into() as u32) }
            pub fn shr_wrap(a: $t, n: impl Into<i128>) -> $t { a.wrapping_shr(n.into() as u32) }
            pub fn to_str(a: $t) -> String { a.to_string() }
            pub fn to_i64_wrap(a: $t) -> i64 { a as i64 }
            pub fn to_u64_wrap(a: $t) -> u64 { a as u64 }
            pub fn to_u8_wrap(a: $t) -> u8 { a as u8 }
            pub fn to_u64(a: $t) -> u64 { a as u64 }
            pub fn to_i64(a: $t) -> i64 { a as i64 }
            pub fn to_f64(a: $t) -> f64 { a as f64 }
        }
    };
}
ints!(ops_i64 i64 ops_u64 u64 ops_u8 u8 ops_i32 i32 ops_u32 u32 ops_u16 u16);

pub mod I64 {
    pub use super::ops_i64::*;
    pub fn abs(a: i64) -> i64 { a.abs() }
    pub fn pow(a: i64, b: i64) -> i64 { a.pow(b as u32) }
    /// Logical shift right: zeros fill from the left.
    pub fn shr_zf_wrap(a: i64, n: impl Into<i128>) -> i64 { ((a as u64).wrapping_shr(n.into() as u32)) as i64 }
}
pub mod U64 {
    pub use super::ops_u64::*;
    pub fn shr_zf_wrap(a: u64, n: impl Into<i128>) -> u64 { a.wrapping_shr(n.into() as u32) }
}
pub mod U8 {
    pub use super::ops_u8::*;
}
pub mod I32 {
    pub use super::ops_i32::*;
}
pub mod U32 {
    pub use super::ops_u32::*;
}
pub mod U16 {
    pub use super::ops_u16::*;
}

pub mod Bool {
    pub fn not(a: bool) -> bool { !a }
}

pub mod F64 {
    pub fn to_bits(a: f64) -> u64 { a.to_bits() }
    pub fn from_bits(a: u64) -> f64 { f64::from_bits(a) }
    pub fn to_i64_wrap(a: f64) -> i64 { a as i64 }
    pub fn is_nan(a: f64) -> bool { a.is_nan() }
    pub fn is_infinite(a: f64) -> bool { a.is_infinite() }
    pub fn abs(a: f64) -> f64 { a.abs() }
    pub fn sqrt(a: f64) -> f64 { a.sqrt() }
    pub fn to_str(a: f64) -> String { format!("{:?}", a) }
}

// ---- the platform ----

pub fn echo(s: String) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(s.as_bytes());
}
/// fasttrack's cli platform: `Echo.line!`.
pub fn echo_line(s: String) {
    echo(s + "\n")
}
