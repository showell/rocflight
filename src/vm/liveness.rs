//! Which register reads are the LAST one, so the value can be taken instead of cloned.
//!
//! `Rc` made a record, a list and a tag cheap to share. It did not make them cheap to
//! *change*, because nothing in the machine ever knew it held the only handle:
//! `Op::Move` cloned, so `p = step(p)` left the caller's binding holding a second
//! handle for the whole call, and `Rc::make_mut` inside `UpdateRecord` therefore always
//! copied. `tests/bench/records.roc` spent 80,720 allocations — two per iteration — on
//! exactly that.
//!
//! roc solves it with ownership: a value at its last use is given away, not lent. This
//! is the interpreter's version, computed rather than declared. A standard backward
//! liveness pass over the flat opcode vector answers "is this register read again after
//! this instruction?", and where the answer is no the read takes the value out of the
//! register instead of cloning it, leaving `Unit` behind. The refcount drops to one and
//! `make_mut` mutates in place.
//!
//! **The soundness rule is one-directional and everything here follows it: reads may be
//! over-approximated and kills may be under-approximated, never the other way round.**
//! An extra read or a missed kill only keeps a register live longer, which costs a take
//! that could have been made. A *missed* read would take a value something still needs
//! and leave it holding `Unit` — a wrong answer, with no crash. That is why `reads`
//! and `kills` below match on every opcode by name with no wildcard arm: a new opcode
//! does not compile until someone has said what it reads.

use super::{Op, Reg};

/// Rewrite the reads this analysis proves are final into taking forms.
///
/// Only `Move` and `UpdateRecord` are rewritten. They are where the measurement said
/// the copies are; every other read is left alone rather than widened on a guess.
pub fn mark_takes(code: &mut [Op], n_regs: u16) {
    // ponytail: skip the analysis for a chunk big enough that the bitsets would
    // matter. Nothing in `Builtin.roc` or any program in the test suite comes close;
    // if one ever does it loses the takes, not its correctness.
    if code.is_empty() || n_regs == 0 || code.len() > 4096 || n_regs > 1024 {
        return;
    }
    let live = solve(code, n_regs);
    let words = words_for(n_regs);
    let mut buf = Vec::new();
    for ip in 0..code.len() {
        let out = live_out(&live, code, words, ip);
        match code[ip] {
            Op::Move { dst, src } if dst != src && !get(&out, src) => {
                code[ip] = Op::MoveTake { dst, src };
            }
            Op::UpdateRecord { dst, obj, name, base, n, take: false } => {
                // The op reads `obj` twice if the record is also one of the field
                // values, and taking it would empty the register before the second
                // read. Only a single read can be a last read.
                let once = !(base..base.saturating_add(n)).contains(&obj);
                if once && !get(&out, obj) {
                    code[ip] = Op::UpdateRecord { dst, obj, name, base, n, take: true };
                }
            }
            _ => {}
        }
        reads(&code[ip], &mut buf);
    }
}

fn words_for(n_regs: u16) -> usize {
    (n_regs as usize).div_ceil(64)
}

fn get(set: &[u64], r: Reg) -> bool {
    let (w, b) = (r as usize / 64, r as usize % 64);
    set.get(w).is_some_and(|word| word >> b & 1 == 1)
}

fn set(bits: &mut [u64], r: Reg) {
    let (w, b) = (r as usize / 64, r as usize % 64);
    if let Some(word) = bits.get_mut(w) {
        *word |= 1 << b;
    }
}

fn clear(bits: &mut [u64], r: Reg) {
    let (w, b) = (r as usize / 64, r as usize % 64);
    if let Some(word) = bits.get_mut(w) {
        *word &= !(1 << b);
    }
}

/// `live_in` for every instruction: the registers that may be read from here on.
///
/// Backward fixpoint. The code is a flat vector, so "the instruction after" is `ip + 1`
/// and a jump adds its target; `successors` is what says which.
fn solve(code: &[Op], n_regs: u16) -> Vec<u64> {
    let words = words_for(n_regs);
    let mut live = vec![0u64; code.len() * words];
    let mut buf = Vec::new();
    let mut scratch = vec![0u64; words];
    loop {
        let mut changed = false;
        for ip in (0..code.len()).rev() {
            scratch.copy_from_slice(&live_out(&live, code, words, ip));
            kills(&code[ip], &mut buf);
            for r in &buf {
                clear(&mut scratch, *r);
            }
            reads(&code[ip], &mut buf);
            for r in &buf {
                set(&mut scratch, *r);
            }
            let slot = &mut live[ip * words..(ip + 1) * words];
            if slot != scratch.as_slice() {
                slot.copy_from_slice(&scratch);
                changed = true;
            }
        }
        if !changed {
            return live;
        }
    }
}

/// The union of `live_in` over this instruction's successors.
fn live_out(live: &[u64], code: &[Op], words: usize, ip: usize) -> Vec<u64> {
    let mut out = vec![0u64; words];
    successors(&code[ip], ip, code.len(), |s| {
        for (o, w) in out.iter_mut().zip(&live[s * words..(s + 1) * words]) {
            *o |= *w;
        }
    });
    out
}

/// Where control may go after this instruction.
///
/// Over-approximate: a successor that cannot really be reached only keeps registers
/// live, which loses a take rather than breaking one. `TailCall` is listed as jumping
/// to the top of the chunk even though a builtin in tail position returns instead.
pub(super) fn successors(op: &Op, ip: usize, len: usize, mut f: impl FnMut(usize)) {
    let next = ip + 1;
    let fall = |f: &mut dyn FnMut(usize)| {
        if next < len {
            f(next)
        }
    };
    match *op {
        // Leaves the chunk: nothing after it in this frame.
        Op::Ret { .. } | Op::NoMatch { .. } | Op::Crash { .. } => {}
        Op::Jump { to } => f(to as usize),
        Op::TailCall { .. } => f(0),
        Op::JumpFalse { to, .. }
        | Op::TestLit { to, .. }
        | Op::TestLitDyn { to, .. }
        | Op::TestStr { to, .. }
        | Op::TestTag { to, .. }
        | Op::TestTuple { to, .. }
        | Op::TestRecord { to, .. }
        | Op::TestList { to, .. }
        | Op::TestBool { to, .. }
        | Op::GetFieldOr { to, .. }
        | Op::IterNext { to, .. }
        | Op::IterNextBack { to, .. } => {
            f(to as usize);
            fall(&mut f);
        }

        // Everything else falls through and nowhere else. Spelled out rather than
        // left to a wildcard: a new BRANCHING opcode swept up by `_` would hide its
        // target, and a missed edge under-approximates liveness, which is the one
        // direction that breaks a program instead of just costing a take.
        Op::LoadK { .. }
        | Op::Move { .. }
        | Op::MoveTake { .. }
        | Op::LoadGlob { .. }
        | Op::StoreGlob { .. }
        | Op::LoadCap { .. }
        | Op::MakeCell { .. }
        | Op::CellGet { .. }
        | Op::CellSet { .. }
        | Op::LoadSelf { .. }
        | Op::Bin { .. }
        | Op::BinInt { .. }
        | Op::BinK { .. }
        | Op::BinIntK { .. }
        | Op::BinDispatch { .. }
        | Op::MakeClosure { .. }
        | Op::CallFn { .. }
        | Op::Call { .. }
        | Op::MakeList { .. }
        | Op::MakeTuple { .. }
        | Op::ListPush { .. }
        | Op::MakeTag { .. }
        | Op::MakeRecord { .. }
        | Op::UpdateRecord { .. }
        | Op::GetField { .. }
        | Op::GetOptField { .. }
        | Op::GetIndex { .. }
        | Op::GetPayload { .. }
        | Op::GetRest { .. }
        | Op::GetElem { .. }
        | Op::GetSlice { .. }
        | Op::MakeRange { .. }
        | Op::CallBuiltin { .. }
        | Op::CallHost { .. }
        | Op::MakeBuiltin { .. }
        | Op::DispatchMethod { .. }
        | Op::Interp { .. }
        | Op::Expect { .. }
        | Op::TestExpect { .. }
        | Op::Dbg { .. } => fall(&mut f),
    }
}

/// Every register this instruction reads. Over-approximating is safe; missing one is
/// not, which is why there is no wildcard arm.
fn reads(op: &Op, out: &mut Vec<Reg>) {
    out.clear();
    let window = |base: Reg, n: u16, out: &mut Vec<Reg>| {
        for i in 0..n {
            out.push(base.saturating_add(i));
        }
    };
    match *op {
        Op::LoadK { .. }
        | Op::LoadGlob { .. }
        | Op::LoadCap { .. }
        | Op::LoadSelf { .. }
        | Op::Jump { .. }
        | Op::MakeBuiltin { .. } => {}

        Op::Move { src, .. }
        | Op::MoveTake { src, .. }
        | Op::StoreGlob { src, .. }
        | Op::MakeCell { src, .. }
        | Op::Ret { src }
        | Op::Crash { src } => out.push(src),
        Op::Dbg { src, shape } => {
            out.push(src);
            out.push(shape);
        }

        Op::CellGet { cell, .. } => out.push(cell),
        Op::CellSet { cell, src } => {
            out.push(cell);
            out.push(src);
        }

        Op::Bin { a, b, .. } | Op::BinInt { a, b, .. } | Op::BinDispatch { a, b, .. } => {
            out.push(a);
            out.push(b);
        }

        // The right operand is a constant, not a register.
        Op::BinK { a, .. } | Op::BinIntK { a, .. } => out.push(a),

        Op::JumpFalse { cond, .. }
        | Op::TestBool { cond, .. }
        | Op::Expect { cond }
        | Op::TestExpect { cond } => out.push(cond),

        Op::MakeClosure { base, n, .. }
        | Op::MakeList { base, n, .. }
        | Op::MakeTuple { base, n, .. }
        | Op::MakeTag { base, n, .. }
        | Op::MakeRecord { base, n, .. }
        | Op::Interp { base, n, .. } => window(base, n, out),

        Op::CallFn { base, argc, .. }
        | Op::CallBuiltin { base, argc, .. }
        | Op::CallHost { base, argc, .. }
        | Op::DispatchMethod { base, argc, .. } => window(base, argc, out),

        Op::Call { func, base, argc, .. } | Op::TailCall { func, base, argc, .. } => {
            out.push(func);
            window(base, argc, out);
        }

        Op::ListPush { list, src } => {
            out.push(list);
            out.push(src);
        }

        Op::UpdateRecord { obj, base, n, .. } => {
            out.push(obj);
            window(base, n, out);
        }

        Op::GetField { obj, .. }
        | Op::GetOptField { obj, .. }
        | Op::GetIndex { obj, .. }
        | Op::GetPayload { obj, .. }
        | Op::GetFieldOr { obj, .. }
        | Op::GetRest { obj, .. }
        | Op::GetElem { obj, .. }
        | Op::GetSlice { obj, .. }
        | Op::TestLit { obj, .. }
        | Op::TestLitDyn { obj, .. }
        | Op::TestStr { obj, .. }
        | Op::TestTag { obj, .. }
        | Op::TestTuple { obj, .. }
        | Op::TestRecord { obj, .. }
        | Op::TestList { obj, .. }
        | Op::NoMatch { obj } => out.push(obj),

        Op::MakeRange { start, end, .. } => {
            out.push(start);
            out.push(end);
        }

        Op::IterNext { iter, idx, .. } | Op::IterNextBack { iter, idx, .. } => {
            out.push(iter);
            out.push(idx);
        }
    }
}

/// The registers this instruction DEFINITELY overwrites.
///
/// Under-approximating is safe: a missed kill keeps a register live and loses a take.
/// Claiming one that is not always written would kill a value still in use, so an op
/// that writes only on one of its outgoing edges — `GetFieldOr`, `IterNext`, `TestStr`
/// — and one that writes a range of unknown length answer nothing.
fn kills(op: &Op, out: &mut Vec<Reg>) {
    out.clear();
    match *op {
        Op::LoadK { dst, .. }
        | Op::Move { dst, .. }
        | Op::MoveTake { dst, .. }
        | Op::LoadGlob { dst, .. }
        | Op::LoadCap { dst, .. }
        | Op::MakeCell { dst, .. }
        | Op::CellGet { dst, .. }
        | Op::LoadSelf { dst }
        | Op::Bin { dst, .. }
        | Op::BinInt { dst, .. }
        | Op::BinK { dst, .. }
        | Op::BinIntK { dst, .. }
        | Op::BinDispatch { dst, .. }
        | Op::MakeClosure { dst, .. }
        | Op::CallFn { dst, .. }
        | Op::Call { dst, .. }
        | Op::MakeList { dst, .. }
        | Op::MakeTuple { dst, .. }
        | Op::MakeTag { dst, .. }
        | Op::MakeRecord { dst, .. }
        | Op::UpdateRecord { dst, .. }
        | Op::GetField { dst, .. }
        | Op::GetOptField { dst, .. }
        | Op::GetIndex { dst, .. }
        | Op::GetPayload { dst, .. }
        | Op::GetRest { dst, .. }
        | Op::GetElem { dst, .. }
        | Op::GetSlice { dst, .. }
        | Op::MakeRange { dst, .. }
        | Op::CallBuiltin { dst, .. }
        | Op::CallHost { dst, .. }
        | Op::MakeBuiltin { dst, .. }
        | Op::DispatchMethod { dst, .. }
        | Op::Interp { dst, .. } => out.push(dst),

        // The arguments move DOWN to this frame's own parameters and execution starts
        // again at the top, so registers `0..argc` hold the new arguments by the time
        // anything reads them. Without this a tail-recursive function's parameters look
        // live all the way round the loop and never get taken, which is most of what
        // this pass is for — `records_tail` is exactly that shape.
        Op::TailCall { argc, .. } => {
            for i in 0..argc {
                out.push(i);
            }
        }

        // Writes nothing, or writes only on some edges / a range of unknown length.
        Op::StoreGlob { .. }
        | Op::CellSet { .. }
        | Op::Jump { .. }
        | Op::JumpFalse { .. }
        | Op::Ret { .. }
        | Op::ListPush { .. }
        | Op::TestLit { .. }
        | Op::TestLitDyn { .. }
        | Op::TestStr { .. }
        | Op::TestTag { .. }
        | Op::TestTuple { .. }
        | Op::TestRecord { .. }
        | Op::TestList { .. }
        | Op::TestBool { .. }
        | Op::NoMatch { .. }
        | Op::GetFieldOr { .. }
        | Op::Expect { .. }
        | Op::TestExpect { .. }
        | Op::Dbg { .. }
        | Op::Crash { .. }
        | Op::IterNext { .. }
        | Op::IterNextBack { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape the whole pass exists for: a value is copied into an argument
    /// register, the call answers, and the answer goes back where the value came from.
    /// The copy is the value's last read, so it may take it.
    #[test]
    fn a_value_passed_and_then_reassigned_is_taken() {
        let mut code = vec![
            Op::Move { dst: 5, src: 1 },
            Op::CallFn { dst: 5, chunk: 0, base: 5, argc: 1 },
            Op::Move { dst: 1, src: 5 },
            Op::Ret { src: 1 },
        ];
        mark_takes(&mut code, 8);
        assert!(matches!(code[0], Op::MoveTake { dst: 5, src: 1 }), "{:?}", code[0]);
    }

    /// The same shape with the value read again afterwards: NOT a last read, and
    /// taking it would leave `Unit` where the second reader looks.
    #[test]
    fn a_value_read_again_later_is_not_taken() {
        let mut code = vec![
            Op::Move { dst: 5, src: 1 },
            Op::CallFn { dst: 5, chunk: 0, base: 5, argc: 1 },
            Op::Ret { src: 1 },
        ];
        mark_takes(&mut code, 8);
        assert!(matches!(code[0], Op::Move { dst: 5, src: 1 }), "{:?}", code[0]);
    }

    /// A backward jump makes the read reachable again, which is what a loop is. The
    /// fixpoint has to see that, or every accumulator in a loop would be emptied.
    #[test]
    fn a_read_reachable_again_round_a_loop_is_not_taken() {
        let mut code = vec![
            Op::Move { dst: 5, src: 1 },   // 0: reads reg 1
            Op::Jump { to: 0 },            // 1: and comes back to read it again
        ];
        mark_takes(&mut code, 8);
        assert!(matches!(code[0], Op::Move { dst: 5, src: 1 }), "{:?}", code[0]);
    }

    /// A record updated and then returned: the source is dead after the update, so
    /// `UpdateRecord` may take it and `Rc::make_mut` mutates in place.
    #[test]
    fn an_updated_record_whose_source_dies_is_taken() {
        let mut code = vec![
            Op::UpdateRecord { dst: 1, obj: 0, name: 0, base: 2, n: 1, take: false },
            Op::Ret { src: 1 },
        ];
        mark_takes(&mut code, 4);
        assert!(matches!(code[0], Op::UpdateRecord { take: true, .. }), "{:?}", code[0]);
    }

    /// A tail call moves its arguments down onto the parameters, so a parameter is
    /// NOT live round the loop — the next iteration's value overwrites it. Missing
    /// that left `records_tail` copying its record 40,000 times.
    #[test]
    fn a_tail_call_overwrites_the_parameters_it_loops_back_to() {
        let mut code = vec![
            Op::GetField { dst: 2, obj: 1, name: 0 },
            Op::UpdateRecord { dst: 3, obj: 1, name: 0, base: 2, n: 1, take: false },
            Op::TailCall { func: 0, chunk: Some(0), base: 2, argc: 2 },
        ];
        mark_takes(&mut code, 8);
        assert!(matches!(code[1], Op::UpdateRecord { take: true, .. }), "{:?}", code[1]);
    }

    /// The same, with the original still wanted afterwards.
    #[test]
    fn an_updated_record_still_in_use_is_not_taken() {
        let mut code = vec![
            Op::UpdateRecord { dst: 1, obj: 0, name: 0, base: 2, n: 1, take: false },
            Op::Ret { src: 0 },
        ];
        mark_takes(&mut code, 4);
        assert!(matches!(code[0], Op::UpdateRecord { take: false, .. }), "{:?}", code[0]);
    }
}
