//! rocflight inside a platform's compiled host.
//!
//! A platform's host is `main`. It builds the program's arguments, calls `roc_main`,
//! and exits with what comes back; everything effectful — `hosted_stdout_line`,
//! `hosted_tty_enable_raw_mode`, all sixty of basic-cli's — is a C function the host
//! defines. This crate is the `roc_main` that host links against: it runs the
//! interpreter on the file `ROCFLIGHT_APP` names and, when the program reaches a
//! hosted function, marshals the call to the host's own code.
//!
//! This is the one place rocflight touches raw memory and foreign code, and the only
//! crate without `#![forbid(unsafe_code)]`. What is unsafe here is small and listed:
//! reading and writing bytes at addresses the host's `roc_alloc` handed out, and
//! calling a function pointer the host's table handed out with the register and stack
//! image `platform::abi::Plan` decided on. Every layout decision, every byte of
//! marshalling and the whole interpreter are the safe library this depends on.
//!
//! Symbols, both directions, are the host's contract (`nm libhost.a`):
//! * the host DEFINES `roc_alloc`, `roc_dealloc`, `roc_dbg`, `roc_expect_failed`,
//!   `roc_crashed` and every `hosted_*`;
//! * the host IMPORTS exactly `roc_main`;
//! * `rocflight_hosted_*` are the dispatch table the driver generates from the
//!   platform's `hosted { … }` block, one C file linked in beside this library.
//!
//! See Learning.md §13.

use std::ffi::{c_char, c_void, CStr};

use rocflight::eval::value::{str_value, Value};
use rocflight::eval::Report;
use rocflight::platform::abi::{Plan, Signature, Slot};
use rocflight::platform::layout::{layout_of, Declarations};
use rocflight::platform::marshal::{read_value, release, write_value, Heap};
use rocflight::platform::hosted;
use rocflight::run::{run_file, Options};
use rocflight::types::Type;

extern "C" {
    fn roc_alloc(length: usize, alignment: usize) -> *mut u8;
    fn roc_dealloc(ptr: *mut u8, alignment: usize);
    fn roc_dbg(bytes: *const u8, len: usize);
    fn roc_expect_failed(bytes: *const u8, len: usize);
    /// The host does not return from this; if it somehow does, `crashed` exits.
    fn roc_crashed(bytes: *const u8, len: usize);
    fn rocflight_hosted_count() -> u32;
    fn rocflight_hosted_name(index: u32) -> *const c_char;
    fn rocflight_hosted_fn(index: u32) -> *const c_void;
}

/// The host's allocator, as `marshal` wants it.
///
/// Unsafe on its face — an address from `roc_alloc` is dereferenced — and sound for
/// the same reason a `RocStr` is: the host allocated exactly the bytes asked for, and
/// `marshal` only reads what a layout says is there.
pub struct HostHeap;

impl Heap for HostHeap {
    fn alloc(&mut self, bytes: usize, align: usize) -> usize {
        unsafe { roc_alloc(bytes, align) as usize }
    }
    fn dealloc(&mut self, block: usize, align: usize) {
        unsafe { roc_dealloc(block as *mut u8, align) }
    }
    fn write(&mut self, addr: usize, bytes: &[u8]) {
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), addr as *mut u8, bytes.len()) }
    }
    fn read(&self, addr: usize, len: usize) -> Vec<u8> {
        unsafe { std::slice::from_raw_parts(addr as *const u8, len).to_vec() }
    }
}

/// The stack image every hosted call is made with. Passed by value, so the ABI puts
/// it on the stack — which is what a 24-byte `Str` argument needs — and a callee that
/// expects fewer bytes never looks past them.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Stack([u64; (Plan::MAX_STACK_BYTES / 8) as usize]);

/// A result in `rax:rdx`.
#[repr(C)]
#[derive(Clone, Copy)]
struct Pair(u64, u64);

/// The two shapes every hosted call takes: six register words and the stack image,
/// returning one register or two. `Plan` decides which words mean what.
type CallOne = unsafe extern "C" fn(u64, u64, u64, u64, u64, u64, Stack) -> u64;
type CallTwo = unsafe extern "C" fn(u64, u64, u64, u64, u64, u64, Stack) -> Pair;

/// The host's function for `symbol`, from the generated table.
pub fn hosted_fn(symbol: &str) -> Option<*const c_void> {
    unsafe {
        (0..rocflight_hosted_count()).find_map(|i| {
            let name = CStr::from_ptr(rocflight_hosted_name(i));
            (name.to_bytes() == symbol.as_bytes()).then(|| rocflight_hosted_fn(i))
        })
    }
}

fn word_at(bytes: &[u8], index: usize) -> u64 {
    let mut word = [0u8; 8];
    let start = index * 8;
    let end = bytes.len().min(start + 8);
    if start < end {
        word[..end - start].copy_from_slice(&bytes[start..end]);
    }
    u64::from_le_bytes(word)
}

/// Call the host's `function` with `sig`'s signature.
///
/// The arguments are the host's to keep, so nothing written for them is released
/// here; the result is ours, read into a `Value` and then given back.
///
/// # Safety
/// `function` must be a hosted function of exactly `sig`'s signature, from the
/// host's table.
pub unsafe fn call(function: *const c_void, sig: &Signature, args: &[Value]) -> Result<Value, String> {
    let plan = Plan::of(sig)?;
    if args.len() != sig.args.len() {
        return Err(format!("hosted function takes {} arguments, got {}", sig.args.len(), args.len()));
    }
    let mut heap = HostHeap;
    let mut registers = [0u64; Plan::MAX_REGISTERS as usize];
    let mut next = 0;
    let mut stack = [0u8; Plan::MAX_STACK_BYTES as usize];
    let mut stack_used = 0;
    let mut result = vec![0u8; sig.ret.size as usize];
    if plan.sret {
        registers[0] = result.as_mut_ptr() as u64;
        next = 1;
    }
    for ((arg, layout), slot) in args.iter().zip(&sig.args).zip(&plan.slots) {
        let mut bytes = vec![0u8; layout.size as usize];
        write_value(arg, layout, &mut heap, &mut bytes)?;
        match slot {
            Slot::Nothing => {}
            Slot::Registers(n) => {
                for i in 0..*n as usize {
                    registers[next] = word_at(&bytes, i);
                    next += 1;
                }
            }
            Slot::Stack => {
                stack[stack_used..stack_used + bytes.len()].copy_from_slice(&bytes);
                stack_used += (bytes.len() + 7) / 8 * 8;
            }
        }
    }
    // Both are plain arrays of the same 512 bytes; `Stack` only exists to be passed
    // by value.
    let image: Stack = std::mem::transmute(stack);
    let [r0, r1, r2, r3, r4, r5] = registers;
    let size = result.len();
    if plan.ret_registers == 2 {
        let f: CallTwo = std::mem::transmute(function);
        let Pair(lo, hi) = f(r0, r1, r2, r3, r4, r5, image);
        let words = [lo.to_le_bytes(), hi.to_le_bytes()].concat();
        result.copy_from_slice(&words[..size]);
    } else {
        let f: CallOne = std::mem::transmute(function);
        let rax = f(r0, r1, r2, r3, r4, r5, image);
        if !plan.sret {
            result.copy_from_slice(&rax.to_le_bytes()[..size]);
        }
    }
    let value = read_value(&result, &sig.ret, &heap)?;
    release(&result, &sig.ret, &mut heap);
    Ok(value)
}

/// `dbg` and a failed inline `expect` are the host's to print: it owns the process,
/// and its `roc_expect_failed` is what turns a clean exit into a failed one.
fn report(kind: Report, message: &str) {
    unsafe {
        match kind {
            Report::Dbg => roc_dbg(message.as_ptr(), message.len()),
            Report::ExpectFailed => roc_expect_failed(message.as_ptr(), message.len()),
        }
    }
}

fn crashed(message: &str) -> i32 {
    unsafe { roc_crashed(message.as_ptr(), message.len()) }
    std::process::exit(1)
}

/// `List(OsStr)`, as the host builds it: what `main!` receives.
#[repr(C)]
pub struct RocList([u64; 3]);

/// `[Utf8(Str), UnixBytes(List(U8)), WindowsU16s(List(U16))]`.
fn os_str() -> Type {
    union(&[("Utf8", &[Type::Str]), ("UnixBytes", &[list(Type::U8)]), ("WindowsU16s", &[list(Type::U16)])])
}

fn union(tags: &[(&'static str, &[Type])]) -> Type {
    let mut tags: Vec<(&'static str, Vec<Type>)> = tags.iter().map(|(n, a)| (*n, a.to_vec())).collect();
    tags.sort_by(|a, b| a.0.cmp(&b.0));
    Type::TagUnion { tags, open: false }
}

fn list(item: Type) -> Type {
    Type::List(Box::new(item))
}

/// What the interpreter calls when a program reaches a hosted effect: look the symbol
/// up in the table and make the call.
fn bridge(symbol: &str, sig: &Signature, args: &[Value]) -> Result<Value, String> {
    let function = hosted_fn(symbol).ok_or_else(|| format!("`{}` is not in the platform's hosted table", symbol))?;
    unsafe { call(function, sig, args) }
}

/// The symbol the host calls. Runs `ROCFLIGHT_APP` with the host's arguments and
/// answers with the exit code.
#[no_mangle]
pub extern "C" fn roc_main(args: RocList) -> i32 {
    rocflight::eval::set_reporter(report);
    hosted::install(bridge);
    if std::env::var_os("ROCFLIGHT_SELFTEST").is_some() {
        return selftest::run();
    }
    let Some(app) = std::env::var_os("ROCFLIGHT_APP") else {
        eprintln!("rocflight: ROCFLIGHT_APP names no file to run");
        return 1;
    };

    // The host's list is ours now: read it, then give it back.
    let bytes: Vec<u8> = args.0.iter().flat_map(|w| w.to_le_bytes()).collect();
    let layout = layout_of(&list(os_str()), &Declarations::default()).expect("List(OsStr) lays out");
    let mut heap = HostHeap;
    let args = match read_value(&bytes, &layout, &heap) {
        Ok(v) => v,
        Err(e) => return crashed(&format!("rocflight: cannot read the arguments: {}", e)),
    };
    release(&bytes, &layout, &mut heap);

    // The platform's own entry — basic-cli's `main_for_host!` — runs `main!` and
    // decides the exit code in Roc; what comes back is that `I32`.
    let options = Options { args: Some(args), host_entry: true, ..Options::default() };
    match run_file(&app.to_string_lossy(), options) {
        Ok(Some(ran)) => match ran.value {
            Value::Int(code) => code as i32,
            // A numeral the checker could not pin to I32 is still a whole number here.
            Value::Float(code) => code as i32,
            Value::F32(code) => code as i32,
            Value::Dec(code) => (code / 1_000_000_000_000_000_000) as i32,
            other => crashed(&format!("the platform's entry point returned {}, not an I32", other)),
        },
        Ok(None) => 0,
        Err(e) => crashed(&format!("{}", e)),
    }
}

/// Calls real hosted functions through `call`, one per ABI shape, and reports.
///
/// This is the only way to prove the register-and-stack image against a callee
/// compiled by another compiler, so `tests/check_host.sh` links this against
/// basic-cli's actual host and runs it with `ROCFLIGHT_SELFTEST=1`.
mod selftest {
    use super::*;

    fn io_err() -> Type {
        union(&[
            ("AlreadyExists", &[]), ("BrokenPipe", &[]), ("Interrupted", &[]),
            ("IsADirectory", &[]), ("NotFound", &[]), ("NotADirectory", &[]),
            ("Other", &[Type::Str]), ("OutOfMemory", &[]), ("PermissionDenied", &[]),
            ("Unsupported", &[]),
        ])
    }

    fn try_ty(ok: Type, err: Type) -> Type {
        union(&[("Ok", &[ok]), ("Err", &[err])])
    }

    fn func(params: &[Type], ret: Type) -> Type {
        params.iter().rev().fold(ret, |r, p| Type::Function(Box::new(p.clone()), Box::new(r)))
    }

    fn check(symbol: &str, ty: Type, args: Vec<Value>) -> Result<Value, String> {
        let function = hosted_fn(symbol).ok_or_else(|| format!("{} is not in the hosted table", symbol))?;
        let sig = Signature::of(&ty, &Declarations::default())?;
        unsafe { call(function, &sig, &args) }
    }

    pub fn run() -> i32 {
        let cases: Vec<(&str, Type, Vec<Value>)> = vec![
            // Stack argument, sret result — the common shape.
            ("hosted_stdout_line",
             func(&[Type::Str], try_ty(Type::Unit, union(&[("StdoutErr", &[io_err()])]))),
             vec![str_value("selftest: a line long enough to live on the heap")]),
            ("hosted_stdout_line",
             func(&[Type::Str], try_ty(Type::Unit, union(&[("StdoutErr", &[io_err()])]))),
             vec![str_value("selftest: inline")]),
            // One register in, nothing out.
            ("hosted_sleep_millis", func(&[Type::U64], Type::Unit), vec![Value::Int(1)]),
            // Nothing in; a 16-byte-aligned union (U128 + discriminant, 32 bytes) out
            // through sret. Getting this signature wrong once meant a segfault, which
            // is the whole argument for reading signatures off Host.roc, not typing them.
            ("hosted_utc_now",
             func(&[Type::Unit], try_ty(Type::U128, union(&[("ClockBeforeEpoch", &[])]))),
             vec![]),
            // A `U64` payload beside a discriminant, through sret. (No basic-cli result
            // is 16 bytes or under with a payload, so the two-register return path is
            // covered by `abi`'s unit tests rather than here.)
            ("hosted_random_seed_u64",
             func(&[Type::Unit], try_ty(Type::U64, union(&[("RandomErr", &[io_err()])]))),
             vec![]),
            // A 32-byte union in, a union of unions with strings inside out.
            ("hosted_env_var",
             func(&[os_str()], try_ty(os_str(), union(&[("VarNotFound", &[os_str()]), ("EnvErr", &[io_err()])]))),
             vec![Value::tag("Utf8", vec![str_value("ROCFLIGHT_SELFTEST")])]),
            ("hosted_env_var",
             func(&[os_str()], try_ty(os_str(), union(&[("VarNotFound", &[os_str()]), ("EnvErr", &[io_err()])]))),
             vec![Value::tag("Utf8", vec![str_value("ROCFLIGHT_NO_SUCH_VARIABLE")])]),
            // A list result, at end of input.
            ("hosted_stdin_bytes",
             func(&[Type::Unit], try_ty(list(Type::U8), union(&[("EndOfFile", &[]), ("StdinErr", &[io_err()])]))),
             vec![]),
        ];
        let mut failed = 0;
        for (symbol, ty, args) in cases {
            match check(symbol, ty, args) {
                Ok(value) => println!("selftest {} -> {}", symbol, value),
                Err(e) => {
                    failed += 1;
                    println!("selftest {} FAILED: {}", symbol, e);
                }
            }
        }
        (failed > 0) as i32
    }
}
