//! How a hosted function's arguments and result travel on x86-64 System V.
//!
//! The host is C-ABI native code and every hosted function has its own signature,
//! known only once the platform is read. `roc` synthesises each call with a fixed
//! assembly trampoline. rocflight does not need to, because the ABI's own rules make
//! every call the same shape:
//!
//! * an argument of 8 bytes or fewer rides in one integer register, 9–16 in two;
//! * anything bigger is copied onto the stack, 8-byte aligned, in argument order;
//! * a result of 8 bytes or fewer comes back in `rax`, 9–16 in `rax:rdx`, and
//!   anything bigger is written through a hidden pointer passed as the FIRST
//!   register argument (`sret`).
//!
//! So a call is: up to six register words, then one contiguous blob of stack bytes.
//! A callee that takes fewer registers, or a shorter blob, ignores the rest — extra
//! arguments cost nothing on this ABI. `host/` makes the actual call with exactly two
//! `extern "C"` function types (one per result register count); this module, which
//! is safe, decides what goes where. `Plan::of` rejects the shapes it cannot do
//! rather than mis-passing them.
//!
//! `ponytail:` x86-64 SysV only. aarch64 differs (`x8` for `sret`, composites in
//! registers by other rules); floating-point arguments and results ride in SSE
//! registers and are rejected here — no basic-cli hosted function has one.

use super::layout::{layout_of, Declarations, Layout, Scalar, Shape};
use crate::types::Type;

/// A hosted function's laid-out parameters and result.
#[derive(Debug, Clone, PartialEq)]
pub struct Signature {
    pub args: Vec<Layout>,
    pub ret: Layout,
}

impl Signature {
    /// Lay out a function type. `A, B => C` arrives curried, so the parameters are
    /// peeled off the front until what is left is the result.
    pub fn of(ty: &Type, decls: &Declarations) -> Result<Signature, String> {
        let mut args = Vec::new();
        let mut rest = ty;
        while let Type::Function(param, ret) = rest {
            args.push(layout_of(param, decls)?);
            rest = ret;
        }
        if args.is_empty() {
            return Err(format!("`{}` is not a function type", ty));
        }
        // `f! : () => X` takes nothing: the `()` is Roc's empty parameter list, and a
        // call `f!()` passes no argument for it.
        if args.len() == 1 && args[0].size == 0 {
            args.clear();
        }
        Ok(Signature { args, ret: layout_of(rest, decls)? })
    }
}

/// Where one argument goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// Zero bytes: `{}`. Nothing is passed.
    Nothing,
    /// This many integer registers (1 or 2).
    Registers(u8),
    /// Copied onto the stack, padded to a multiple of 8.
    Stack,
}

/// How one signature's call is laid out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub slots: Vec<Slot>,
    /// The result is written through a pointer passed as the first register.
    pub sret: bool,
    /// Integer registers the result comes back in when not `sret`: 0, 1 or 2.
    pub ret_registers: u8,
    /// Register words used in total, `sret` included. At most six.
    pub registers: u8,
    /// Bytes on the stack in total.
    pub stack_bytes: u32,
}

/// Registers a value of this size occupies when it is not passed on the stack.
fn registers_for(size: u32) -> u8 {
    if size <= 8 { 1 } else { 2 }
}

fn has_float(layout: &Layout) -> bool {
    match &layout.shape {
        Shape::Scalar(Scalar::F32 | Scalar::F64) => true,
        Shape::Scalar(_) | Shape::Str | Shape::Box | Shape::Zst => false,
        Shape::List(_) => false,
        Shape::Struct(fields) | Shape::Tuple(fields) => fields.iter().any(|f| has_float(&f.layout)),
        Shape::TagUnion { tags, .. } => tags.iter().any(|t| has_float(&t.payload)),
    }
}

impl Plan {
    pub const MAX_REGISTERS: u8 = 6;
    /// The blob `host/` passes: room for basic-cli's 160-byte `Cmd` three times over.
    pub const MAX_STACK_BYTES: u32 = 512;

    pub fn of(sig: &Signature) -> Result<Plan, String> {
        let mut slots = Vec::with_capacity(sig.args.len());
        let mut registers: u8 = 0;
        let mut stack_bytes: u32 = 0;

        let (sret, ret_registers) = match sig.ret.size {
            0 => (false, 0),
            1..=16 => {
                if has_float(&sig.ret) {
                    return Err("a floating-point result rides in SSE registers, which this does not do".into());
                }
                (false, registers_for(sig.ret.size))
            }
            _ => (true, 0),
        };
        if sret {
            registers += 1;
        }

        for arg in &sig.args {
            let slot = match arg.size {
                0 => Slot::Nothing,
                1..=16 => {
                    if has_float(arg) {
                        return Err("a floating-point argument rides in SSE registers, which this does not do".into());
                    }
                    let n = registers_for(arg.size);
                    registers += n;
                    Slot::Registers(n)
                }
                _ => {
                    // The stack blob is 8-aligned; a 16-aligned argument would need
                    // padding the callee also expects, and none has come up.
                    if arg.align > 8 {
                        return Err(format!("a {}-byte-aligned argument on the stack is not supported", arg.align));
                    }
                    stack_bytes += (arg.size + 7) / 8 * 8;
                    Slot::Stack
                }
            };
            slots.push(slot);
        }

        if registers > Self::MAX_REGISTERS {
            return Err(format!("{} register words needed, at most {} fit", registers, Self::MAX_REGISTERS));
        }
        if stack_bytes > Self::MAX_STACK_BYTES {
            return Err(format!("{} stack bytes needed, at most {} fit", stack_bytes, Self::MAX_STACK_BYTES));
        }
        Ok(Plan { slots, sret, ret_registers, registers, stack_bytes })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn union_ty(tags: &[(&str, &[Type])]) -> Type {
        let mut tags: Vec<(&'static str, Vec<Type>)> = tags.iter().map(|(n, a)| (crate::memory::string_pool::intern(n), a.to_vec())).collect();
        tags.sort_by(|a, b| a.0.cmp(&b.0));
        Type::TagUnion { tags, open: false, row: None }
    }

    fn func(params: &[Type], ret: Type) -> Type {
        params.iter().rev().fold(ret, |r, p| Type::Function(Box::new(p.clone()), Box::new(r)))
    }

    fn io_err() -> Type {
        union_ty(&[("NotFound", &[]), ("Other", &[Type::Str]), ("PermissionDenied", &[])])
    }

    fn plan(ty: &Type) -> Plan {
        Plan::of(&Signature::of(ty, &Declarations::default()).unwrap()).unwrap()
    }

    #[test]
    fn a_str_goes_on_the_stack_and_a_small_try_comes_back_in_a_register() {
        // `stdout_line! : Str => Try({}, [StdoutErr(IOErr)])` — the result is 40 bytes,
        // so it is sret; the 24-byte Str is stack.
        let p = plan(&func(&[Type::Str], union_ty(&[("Ok", &[Type::Unit]), ("Err", &[union_ty(&[("StdoutErr", &[io_err()])])])])));
        assert_eq!(p.slots, [Slot::Stack]);
        assert!(p.sret);
        assert_eq!((p.registers, p.stack_bytes), (1, 24));
    }

    #[test]
    fn scalars_ride_in_registers() {
        // `sleep_millis! : U64 => {}`
        let p = plan(&func(&[Type::U64], Type::Unit));
        assert_eq!(p, Plan { slots: vec![Slot::Registers(1)], sret: false, ret_registers: 0, registers: 1, stack_bytes: 0 });
        // `() => I128` — two result registers, and `()` is no parameter at all.
        let p = plan(&func(&[Type::Unit], Type::I128));
        assert_eq!(p, Plan { slots: vec![], sret: false, ret_registers: 2, registers: 0, stack_bytes: 0 });
        // A boxed handle is one register.
        let handle = Type::Nominal { name: "Box".into(), backing: Box::new(Type::TypeVar(u32::MAX)), args: Vec::new() };
        let p = plan(&func(&[handle, Type::List(Box::new(Type::U8))], Type::Unit));
        assert_eq!(p.slots, [Slot::Registers(1), Slot::Stack]);
    }

    #[test]
    fn floats_and_wide_alignment_are_refused() {
        assert!(Plan::of(&Signature::of(&func(&[Type::F64], Type::Unit), &Declarations::default()).unwrap()).is_err());
        let wide = Type::closed_record(vec![("a".into(), Type::Dec), ("b".into(), Type::Dec)]);
        assert!(Plan::of(&Signature::of(&func(&[wide], Type::Unit), &Declarations::default()).unwrap()).is_err());
        assert!(Signature::of(&Type::Str, &Declarations::default()).is_err());
    }
}
