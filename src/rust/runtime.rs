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
    /// Bytes allocated and not yet freed, and the most there ever were.
    static LIVE: AtomicU64 = AtomicU64::new(0);
    static PEAK: AtomicU64 = AtomicU64::new(0);
    fn grow(by: u64) {
        let now = LIVE.fetch_add(by, Relaxed) + by;
        PEAK.fetch_max(now, Relaxed);
    }
    /// A list written by a builtin: taken whole (held only here), or copied.
    pub static TAKEN: AtomicU64 = AtomicU64::new(0);
    pub static COPIED: AtomicU64 = AtomicU64::new(0);
    /// Lists given an allocation of their own (the `Rc` apart from its elements).
    pub static BOXES: AtomicU64 = AtomicU64::new(0);
    /// Copies by element type: how many, and how many elements in all.
    static COPIES: std::sync::Mutex<Option<std::collections::HashMap<&'static str, (u64, u64)>>> = std::sync::Mutex::new(None);
    /// Copies by the line of the program that caused them.
    static SITES: std::sync::Mutex<Option<std::collections::HashMap<u32, u64>>> = std::sync::Mutex::new(None);
    #[track_caller]
    pub fn copied<T>(len: usize) {
        COPIED.fetch_add(1, Relaxed);
        let mut c = COPIES.lock().unwrap();
        let e = c.get_or_insert_with(Default::default).entry(std::any::type_name::<T>()).or_default();
        e.0 += 1;
        e.1 += len as u64;
        let line = std::panic::Location::caller().line();
        *SITES.lock().unwrap().get_or_insert_with(Default::default).entry(line).or_default() += 1;
    }
    fn report_sites() {
        let c = SITES.lock().unwrap();
        let mut v: Vec<_> = c.iter().flatten().map(|(k, v)| (*k, *v)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        for (line, n) in v.iter().take(15) {
            eprintln!("  copied {:>8} times at line {}", n, line);
        }
    }
    fn report_copies() {
        let c = COPIES.lock().unwrap();
        let mut v: Vec<_> = c.iter().flatten().map(|(k, v)| (*k, *v)).collect();
        v.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
        for (k, (n, len)) in v.iter().take(12) {
            eprintln!("  copied {:>8} times, {:>9} elements: {}", n, len, k.replace("roc::", ""));
        }
    }
    pub struct Counting;
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            ALLOCS.fetch_add(1, Relaxed);
            BYTES.fetch_add(layout.size() as u64, Relaxed);
            grow(layout.size() as u64);
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            LIVE.fetch_sub(layout.size() as u64, Relaxed);
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            REALLOCS.fetch_add(1, Relaxed);
            BYTES.fetch_add(new_size as u64, Relaxed);
            LIVE.fetch_sub(layout.size() as u64, Relaxed);
            grow(new_size as u64);
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }
    #[global_allocator]
    static COUNTING: Counting = Counting;
    pub fn report() {
        eprintln!("allocs {} reallocs {} bytes {}", ALLOCS.load(Relaxed), REALLOCS.load(Relaxed), BYTES.load(Relaxed));
        eprintln!("peak live bytes {}", PEAK.load(Relaxed));
        eprintln!("lists written: taken {} copied {}; list boxes {}", TAKEN.load(Relaxed), COPIED.load(Relaxed), BOXES.load(Relaxed));
        report_copies();
        report_sites();
    }
}

#[derive(Clone)]
pub struct List<T>(Option<Rc<Vec<T>>>);

/// The empty list, whatever the element: no `T: Default` needed, which a derive
/// would ask for (and `mem::take` of a list field relies on).
impl<T> Default for List<T> {
    fn default() -> Self {
        List(None)
    }
}

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
        #[cfg(roc2rust_count_allocs)]
        if !v.is_empty() {
            alloc_count::BOXES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        List(if v.is_empty() { None } else { Some(Rc::new(v)) })
    }
    /// The elements, to write: in place when this list holds the only reference to
    /// them, a copy otherwise (`Rc::make_mut`) -- Roc's rule for a list. The list
    /// keeps its allocation either way.
    #[cfg_attr(roc2rust_count_allocs, track_caller)]
    fn edit(&mut self) -> &mut Vec<T> {
        #[cfg(roc2rust_count_allocs)]
        if self.0.as_ref().map_or(true, |rc| Rc::strong_count(rc) > 1) {
            alloc_count::BOXES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        let rc = self.0.get_or_insert_with(|| Rc::new(Vec::new()));
        #[cfg(roc2rust_count_allocs)]
        {
            if Rc::strong_count(rc) == 1 {
                alloc_count::TAKEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            } else {
                alloc_count::copied::<T>(rc.len());
            }
        }
        Rc::make_mut(rc)
    }
    /// `edit`, for a write that adds `extra` elements: a copy is made with
    /// room for them, not grown again at once.
    #[cfg_attr(roc2rust_count_allocs, track_caller)]
    fn edit_growing(&mut self, extra: usize) -> &mut Vec<T> {
        let shared = self.0.as_ref().map_or(false, |rc| Rc::strong_count(rc) > 1);
        if !shared {
            return self.edit();
        }
        let rc = self.0.as_mut().unwrap();
        #[cfg(roc2rust_count_allocs)]
        {
            alloc_count::BOXES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            alloc_count::copied::<T>(rc.len());
        }
        let mut v = Vec::with_capacity(rc.len() + extra);
        v.extend_from_slice(rc);
        *rc = Rc::new(v);
        Rc::make_mut(rc)
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

pub fn List__len<T: Clone>(l: &List<T>) -> u64 {
    l.items().len() as u64
}
pub fn List__get<T: Clone>(l: &List<T>, i: u64) -> Result<T, ()> {
    l.items().get(i as usize).cloned().ok_or(())
}
#[cfg_attr(roc2rust_count_allocs, track_caller)]
pub fn List__set<T: Clone>(mut l: List<T>, i: u64, x: T) -> Result<List<T>, ()> {
    let i = i as usize;
    if i >= l.items().len() {
        return Err(());
    }
    l.edit()[i] = x;
    Ok(l)
}
/// `List.set(l, i, x) ?? l`: set in range, else the list as it was.
#[cfg_attr(roc2rust_count_allocs, track_caller)]
pub fn List__set_or_same<T: Clone>(mut l: List<T>, i: u64, x: T) -> List<T> {
    let i = i as usize;
    if i < l.items().len() {
        l.edit()[i] = x;
    }
    l
}
#[cfg_attr(roc2rust_count_allocs, track_caller)]
pub fn List__replace<T: Clone>(mut l: List<T>, i: u64, x: T) -> (List<T>, T) {
    let i = i as usize;
    if i >= l.items().len() {
        return (l, x);
    }
    let old = std::mem::replace(&mut l.edit()[i], x);
    (l, old)
}
#[cfg_attr(roc2rust_count_allocs, track_caller)]
pub fn List__insert<T: Clone>(mut l: List<T>, i: u64, x: T) -> Result<List<T>, ()> {
    let i = i as usize;
    if i > l.items().len() {
        return Err(());
    }
    l.edit().insert(i, x);
    Ok(l)
}
#[cfg_attr(roc2rust_count_allocs, track_caller)]
pub fn List__append<T: Clone>(mut l: List<T>, x: T) -> List<T> {
    l.edit_growing(1).push(x);
    l
}
#[cfg_attr(roc2rust_count_allocs, track_caller)]
pub fn List__concat<T: Clone>(mut a: List<T>, b: &List<T>) -> List<T> {
    if !b.is_empty() {
        a.edit_growing(b.items().len()).extend(b.items().iter().cloned());
    }
    a
}
pub fn List__with_capacity<T: Clone>(n: u64) -> List<T> {
    List::of(Vec::with_capacity(n as usize))
}
pub fn List__repeat<T: Clone>(x: T, n: u64) -> List<T> {
    List::of(vec![x; n as usize])
}
#[cfg_attr(roc2rust_count_allocs, track_caller)]
pub fn List__sublist<T: Clone>(mut l: List<T>, r: Rec_len_start) -> List<T> {
    let len = l.items().len();
    let start = (r.start as usize).min(len);
    let end = start.saturating_add(r.len as usize).min(len);
    if start == 0 && end == len {
        return l;
    }
    let v = l.edit();
    v.truncate(end);
    v.drain(..start);
    l
}
#[cfg_attr(roc2rust_count_allocs, track_caller)]
pub fn List__drop_first<T: Clone>(mut l: List<T>, n: u64) -> List<T> {
    let n = (n as usize).min(l.items().len());
    if n > 0 {
        l.edit().drain(..n);
    }
    l
}
#[cfg_attr(roc2rust_count_allocs, track_caller)]
pub fn List__drop_last<T: Clone>(mut l: List<T>, n: u64) -> List<T> {
    let keep = l.items().len().saturating_sub(n as usize);
    if keep < l.items().len() {
        l.edit().truncate(keep);
    }
    l
}
pub fn List__last<T: Clone>(l: &List<T>) -> Result<T, ()> {
    l.items().last().cloned().ok_or(())
}
pub fn List__map<T: Clone, U: Clone>(l: &List<T>, f: &dyn Fn(T) -> U) -> List<U> {
    List::of(l.items().iter().cloned().map(|x| f(x)).collect())
}
pub fn List__fold<T: Clone, S: Clone>(l: &List<T>, init: S, f: &dyn Fn(S, T) -> S) -> S {
    let mut acc = init;
    for x in l.items().iter().cloned() {
        acc = f(acc, x);
    }
    acc
}
pub fn List__starts_with<T: Clone + PartialEq>(l: &List<T>, p: &List<T>) -> bool {
    l.items().starts_with(p.items())
}
pub fn List__ends_with<T: Clone + PartialEq>(l: &List<T>, p: &List<T>) -> bool {
    l.items().ends_with(p.items())
}

pub fn List__is_empty<T: Clone>(l: &List<T>) -> bool {
    l.items().is_empty()
}
pub fn List__first<T: Clone>(l: &List<T>) -> Result<T, ()> {
    l.items().first().cloned().ok_or(())
}
#[cfg_attr(roc2rust_count_allocs, track_caller)]
pub fn List__prepend<T: Clone>(mut l: List<T>, x: T) -> List<T> {
    l.edit_growing(1).insert(0, x);
    l
}
#[cfg_attr(roc2rust_count_allocs, track_caller)]
pub fn List__take_first<T: Clone>(mut l: List<T>, n: u64) -> List<T> {
    let n = n as usize;
    if n < l.items().len() {
        l.edit().truncate(n);
    }
    l
}
#[cfg_attr(roc2rust_count_allocs, track_caller)]
pub fn List__drop_at<T: Clone>(mut l: List<T>, i: u64) -> List<T> {
    let i = i as usize;
    if i < l.items().len() {
        l.edit().remove(i);
    }
    l
}
pub fn List__contains<T: Clone + PartialEq>(l: &List<T>, x: T) -> bool {
    l.items().contains(&x)
}
pub fn List__any<T: Clone>(l: &List<T>, f: &dyn Fn(T) -> bool) -> bool {
    l.items().iter().cloned().any(|x| f(x))
}
pub fn List__all<T: Clone>(l: &List<T>, f: &dyn Fn(T) -> bool) -> bool {
    l.items().iter().cloned().all(|x| f(x))
}
pub fn List__count_if<T: Clone>(l: &List<T>, f: &dyn Fn(T) -> bool) -> u64 {
    l.items().iter().cloned().filter(|x| f(x.clone())).count() as u64
}
#[cfg_attr(roc2rust_count_allocs, track_caller)]
pub fn List__keep_if<T: Clone>(mut l: List<T>, f: &dyn Fn(T) -> bool) -> List<T> {
    if l.items().iter().all(|x| f(x.clone())) {
        return l;
    }
    l.edit().retain(|x| f(x.clone()));
    l
}
#[cfg_attr(roc2rust_count_allocs, track_caller)]
pub fn List__drop_if<T: Clone>(mut l: List<T>, f: &dyn Fn(T) -> bool) -> List<T> {
    if !l.items().iter().any(|x| f(x.clone())) {
        return l;
    }
    l.edit().retain(|x| !f(x.clone()));
    l
}
pub fn List__find_first<T: Clone>(l: &List<T>, f: &dyn Fn(T) -> bool) -> Result<T, ()> {
    l.items().iter().cloned().find(|x| f(x.clone())).ok_or(())
}
pub fn List__find_last<T: Clone>(l: &List<T>, f: &dyn Fn(T) -> bool) -> Result<T, ()> {
    l.items().iter().rev().cloned().find(|x| f(x.clone())).ok_or(())
}
pub fn List__find_first_index<T: Clone>(l: &List<T>, f: &dyn Fn(T) -> bool) -> Result<u64, ()> {
    l.items().iter().cloned().position(|x| f(x)).map(|i| i as u64).ok_or(())
}
pub fn List__map_with_index<T: Clone, U: Clone>(l: &List<T>, f: &dyn Fn(T, u64) -> U) -> List<U> {
    List::of(l.items().iter().cloned().enumerate().map(|(i, x)| f(x, i as u64)).collect())
}
pub fn List__join<T: Clone>(l: &List<List<T>>) -> List<T> {
    let mut v = Vec::with_capacity(l.items().iter().map(|x| x.items().len()).sum());
    for x in l.items() {
        v.extend_from_slice(x.items());
    }
    List::of(v)
}
pub fn List__join_map<T: Clone, U: Clone>(l: &List<T>, f: &dyn Fn(T) -> List<U>) -> List<U> {
    let mut v = Vec::new();
    for x in l.items() {
        v.extend_from_slice(f(x.clone()).items());
    }
    List::of(v)
}
/// `List.sort_with`: a stable sort, as Roc's is, by a comparison answering the
/// program's own `[Before, Same, After]`.
#[cfg_attr(roc2rust_count_allocs, track_caller)]
pub fn List__sort_with<T: Clone, O: RocOrder>(mut l: List<T>, f: &dyn Fn(T, T) -> O) -> List<T> {
    l.edit().sort_by(|a, b| f(a.clone(), b.clone()).order());
    l
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

/// A Roc `Str`, laid out as roc lays it out: up to 23 bytes held in place, a
/// literal pointed at where it lies, and anything longer behind one reference
/// count. Only a string longer than 23 bytes that the program builds allocates.
/// A `Str` is 24 bytes, as roc's is: a small string's length is a `SmallLen`,
/// whose unused values tell the other two apart.
#[derive(Clone)]
pub enum Str {
    Small(SmallLen, [u8; SMALL]),
    Static(&'static str),
    Big(Rc<str>),
}
const SMALL: usize = 23;

/// The length of a small string, 0 to 23.
#[derive(Clone, Copy)]
#[repr(u8)]
pub enum SmallLen {
    L0, L1, L2, L3, L4, L5, L6, L7, L8, L9, L10, L11,
    L12, L13, L14, L15, L16, L17, L18, L19, L20, L21, L22, L23,
}
const SMALL_LENS: [SmallLen; SMALL + 1] = {
    use SmallLen::*;
    [L0, L1, L2, L3, L4, L5, L6, L7, L8, L9, L10, L11, L12, L13, L14, L15, L16, L17, L18, L19, L20, L21, L22, L23]
};

impl Str {
    pub const fn lit(s: &'static str) -> Str {
        Str::Static(s)
    }
    pub fn as_str(&self) -> &str {
        match self {
            // Only `StrBuf` writes a `Small`, and only whole `&str`s into it.
            Str::Small(n, b) => unsafe { std::str::from_utf8_unchecked(&b[..*n as usize]) },
            Str::Static(s) => s,
            Str::Big(s) => s,
        }
    }
}
impl Default for Str {
    fn default() -> Str {
        Str::Static("")
    }
}
impl std::ops::Deref for Str {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}
impl From<&str> for Str {
    fn from(s: &str) -> Str {
        let mut b = StrBuf::new();
        b.push(s);
        b.finish()
    }
}
impl From<String> for Str {
    fn from(s: String) -> Str {
        if s.len() <= SMALL { Str::from(s.as_str()) } else { Str::Big(Rc::from(s)) }
    }
}
impl PartialEq for Str {
    fn eq(&self, o: &Str) -> bool {
        self.as_str() == o.as_str()
    }
}
impl Eq for Str {}
impl PartialEq<&str> for Str {
    fn eq(&self, o: &&str) -> bool {
        self.as_str() == *o
    }
}
impl PartialEq<Str> for &str {
    fn eq(&self, o: &Str) -> bool {
        *self == o.as_str()
    }
}
impl PartialOrd for Str {
    fn partial_cmp(&self, o: &Str) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Str {
    fn cmp(&self, o: &Str) -> std::cmp::Ordering {
        self.as_str().cmp(o.as_str())
    }
}
impl std::hash::Hash for Str {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        self.as_str().hash(h)
    }
}
impl std::fmt::Debug for Str {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.as_str(), f)
    }
}
impl std::fmt::Display for Str {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A `Str` being built: in place while it fits, on the heap once it does not.
pub enum StrBuf {
    Small(u8, [u8; SMALL]),
    Big(String),
}
impl StrBuf {
    pub fn new() -> StrBuf {
        StrBuf::Small(0, [0; SMALL])
    }
    pub fn push(&mut self, s: &str) {
        match self {
            StrBuf::Small(n, b) if *n as usize + s.len() <= SMALL => {
                b[*n as usize..*n as usize + s.len()].copy_from_slice(s.as_bytes());
                *n += s.len() as u8;
            }
            StrBuf::Small(n, b) => {
                let mut t = String::with_capacity(*n as usize + s.len());
                t.push_str(unsafe { std::str::from_utf8_unchecked(&b[..*n as usize]) });
                t.push_str(s);
                *self = StrBuf::Big(t);
            }
            StrBuf::Big(t) => t.push_str(s),
        }
    }
    pub fn finish(self) -> Str {
        match self {
            StrBuf::Small(n, b) => Str::Small(SMALL_LENS[n as usize], b),
            StrBuf::Big(t) => Str::Big(Rc::from(t)),
        }
    }
}
impl std::fmt::Write for StrBuf {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.push(s);
        Ok(())
    }
}
/// A value's text, as `Num.to_str` gives it, built in place.
pub fn display_str(x: impl std::fmt::Display) -> Str {
    let mut b = StrBuf::new();
    let _ = std::fmt::Write::write_fmt(&mut b, format_args!("{}", x));
    b.finish()
}

pub fn Str__concat(a: &Str, b: &Str) -> Str {
    let mut t = StrBuf::new();
    t.push(&a);
    t.push(&b);
    t.finish()
}
pub fn Str__to_utf8(s: &Str) -> List<u8> {
    List::of(s.as_bytes().to_vec())
}
pub fn Str__from_utf8_lossy(l: &List<u8>) -> Str {
    Str::from(String::from_utf8_lossy(l.items()).into_owned())
}

pub fn Str__join_with(l: &List<Str>, sep: &Str) -> Str {
    let mut t = StrBuf::new();
    for (i, s) in l.items().iter().enumerate() {
        if i > 0 {
            t.push(&sep);
        }
        t.push(s);
    }
    t.finish()
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
            pub fn to_str(a: $t) -> super::Str { super::display_str(a) }
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
    pub fn to_str(a: f64) -> super::Str { super::display_str(format_args!("{:?}", a)) }
}

// ---- the platform ----

pub fn echo(s: Str) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(s.as_bytes());
}
/// fasttrack's cli platform: `Echo.line!`.
pub fn echo_line(s: Str) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(s.as_bytes());
    let _ = out.write_all(b"\n");
}
