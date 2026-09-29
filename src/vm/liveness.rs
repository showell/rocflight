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

use super::{CondKind, Op, Reg};

/// Rewrite the reads this analysis proves are final into taking forms.
///
/// Only `Move` and `UpdateRecord` are rewritten. They are where the measurement said
/// the copies are; every other read is left alone rather than widened on a guess.
///
/// Also answers the registers to clear on leaving a branch (`Chunk::drops`): one that
/// only the OTHER edge still needs. Clearing a dead register is always sound, since
/// nothing reads it before writing it; not clearing it kept a value shared that the
/// taken branch wanted to change in place. `Set.insert`'s `Found(_) => set` arm keeps
/// `set` live, and without this the `Missing` arm's insert copied the whole table.
pub fn mark_takes(code: &mut [Op], n_regs: u16, names: &[&str]) -> Vec<(Vec<Reg>, Vec<Reg>)> {
    let mut drops = Vec::new();
    // ponytail: skip the analysis for a chunk big enough that the bitsets would
    // matter. Nothing in `Builtin.roc` or any program in the test suite comes close;
    // if one ever does it loses the takes, not its correctness.
    if code.is_empty() || n_regs == 0 || code.len() > 4096 || n_regs > 1024 {
        return drops;
    }
    let live = solve(code, n_regs);
    let targets = jump_targets(code);
    let words = words_for(n_regs);
    let mut buf = Vec::new();
    for ip in 0..code.len() {
        let out = live_out(&live, code, words, ip);
        match code[ip] {
            Op::Move { dst, src } if dst != src && !get(&out, src) => {
                code[ip] = Op::MoveTake { dst, src };
            }
            // A field or payload read out of a value whose OTHER parts may still be
            // wanted: the register stays live, so this is per component, not the
            // register-level question above. See `component_dead_after`.
            Op::GetField { dst, obj, name } if field_dead_after(code, ip, obj, names, name) => {
                code[ip] = Op::TakeField { dst, obj, name };
            }
            Op::GetFieldOr { dst, obj, name, to } if field_dead_after(code, ip, obj, names, name) => {
                code[ip] = Op::TakeFieldOr { dst, obj, name, to };
            }
            Op::GetPayload { dst, obj, i }
                if component_dead_after(code, ip, obj, names, Component::Payload(i), proven_tag(code, &targets, ip, obj)) =>
            {
                code[ip] = Op::TakePayload { dst, obj, i };
            }
            Op::GetIndex { dst, obj, i } if component_dead_after(code, ip, obj, names, Component::Index(i), None) => {
                code[ip] = Op::TakeIndex { dst, obj, i };
            }
            Op::TestTag { obj, name, n, to } => {
                if let Some(drop) = branch_drops(&live, words, n_regs, ip, to as usize, code.len(), &mut drops) {
                    code[ip] = Op::TestTagDrop { obj, name, n, to, drop };
                }
            }
            // Not a `while` condition, which runs every time round a loop, nor an
            // `&&`/`||` operand: the branches of an `if` and a guard are what can hold
            // a value only the other side wants.
            Op::JumpFalse { cond, to, kind: kind @ (CondKind::If | CondKind::Guard) } => {
                if let Some(drop) = branch_drops(&live, words, n_regs, ip, to as usize, code.len(), &mut drops) {
                    code[ip] = Op::JumpFalseDrop { cond, to, kind, drop };
                }
            }
            Op::UpdateRecord { dst, obj, name, base, n, take: false } => {
                // The op reads `obj` twice if the record is also one of the field
                // values, and taking it would empty the register before the second
                // read. Only a single read can be a last read. Writing the result
                // back over `obj` ends the old value there whatever is live after.
                let once = !(base..base.saturating_add(n)).contains(&obj);
                if once && (dst == obj || !get(&out, obj)) {
                    code[ip] = Op::UpdateRecord { dst, obj, name, base, n, take: true };
                }
            }
            _ => {}
        }
        reads(&code[ip], &mut buf);
    }
    drops
}

/// For the branch at `ip`: the registers live into one successor and dead into the
/// other, as (falling through, jumping), appended to `drops`. The op's `drop` value is
/// its index plus one; `None` when neither edge has any, or the table is full.
fn branch_drops(live: &[u64], words: usize, n_regs: u16, ip: usize, to: usize, len: usize, drops: &mut Vec<(Vec<Reg>, Vec<Reg>)>) -> Option<u16> {
    let fall = ip + 1;
    if fall >= len || to >= len || to == fall {
        return None;
    }
    let live_in = |at: usize| &live[at * words..(at + 1) * words];
    let only = |mine: &[u64], other: &[u64]| -> Vec<Reg> {
        (0..n_regs).filter(|r| get(other, *r) && !get(mine, *r)).collect()
    };
    let on_fall = only(live_in(fall), live_in(to));
    let on_jump = only(live_in(to), live_in(fall));
    if on_fall.is_empty() && on_jump.is_empty() {
        return None;
    }
    let index = u16::try_from(drops.len() + 1).ok()?;
    drops.push((on_fall, on_jump));
    Some(index)
}

/// A part of a value that a take can move out on its own.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Component<'a> {
    Field(&'a str),
    Payload(u16),
    Index(u16),
}

fn field_dead_after(code: &[Op], ip: usize, obj: Reg, names: &[&str], name: u16) -> bool {
    names
        .get(name as usize)
        .is_some_and(|field| component_dead_after(code, ip, obj, names, Component::Field(field), None))
}

/// The tag a `GetPayload` at `ip` is known to be reading: the `TestTag` on the same
/// register that falls straight into it, possibly through the payload reads before it.
/// Only claimed when no jump lands anywhere in between, so every way into `ip` passed
/// that test.
fn proven_tag(code: &[Op], targets: &[bool], ip: usize, obj: Reg) -> Option<u16> {
    let mut at = ip;
    loop {
        at = at.checked_sub(1)?;
        match code[at] {
            Op::GetPayload { obj: o, dst, .. } | Op::TakePayload { obj: o, dst, .. } if o == obj && dst != obj => continue,
            Op::TestTag { obj: o, name, .. } | Op::TestTagDrop { obj: o, name, .. } if o == obj => {
                let entered = targets[at + 1..=ip].iter().any(|t| *t);
                return (!entered).then_some(name);
            }
            _ => return None,
        }
    }
}

/// Which instructions some other instruction jumps to (rather than falls into).
fn jump_targets(code: &[Op]) -> Vec<bool> {
    let mut targets = vec![false; code.len()];
    for (ip, op) in code.iter().enumerate() {
        successors(op, ip, code.len(), |s| {
            if s != ip + 1 {
                targets[s] = true;
            }
        });
    }
    targets
}

/// Is `part` of the value in `obj` never read again after `ip`, on any path, before
/// `obj` is overwritten? Then the read at `ip` may move it out (when the value is not
/// shared at run time, which the machine checks).
///
/// A forward search rather than the backward bitset pass above, because the question
/// is about part of a register while the rest of it stays live, and because one
/// refinement needs the path: after `TestTag` proved the tag (`tag`), a later
/// `TestTag` on the same register naming a DIFFERENT tag cannot succeed, so only its
/// failure edge is followed. That is what lets `match` take a payload even though
/// every arm's failure edge leads to the next arm's test of the same value.
///
/// Same soundness rule as the rest of this file: anything not recognised as reading
/// another part of the value is a read of all of it. `TestTag` and `TestRecord` read
/// only the shape, which a take leaves alone. `NoMatch` is let through too: it reads
/// the value only to print it in the error it is about to raise, and it is reached
/// after a payload take only when a later part of the same pattern fails, which a
/// well-typed program cannot do.
fn component_dead_after(code: &[Op], ip: usize, obj: Reg, names: &[&str], part: Component, tag: Option<u16>) -> bool {
    // The instruction itself overwrites the register: nothing can read the rest.
    if matches!(
        code[ip],
        Op::GetField { dst, .. }
            | Op::GetPayload { dst, .. }
            | Op::GetIndex { dst, .. }
            | Op::TakeField { dst, .. }
            | Op::TakePayload { dst, .. }
            | Op::TakeIndex { dst, .. }
            if dst == obj
    ) {
        return true;
    }
    let name = |i: u16| names.get(i as usize).copied();
    let mut seen = vec![false; code.len()];
    let mut stack = Vec::new();
    let mut buf = Vec::new();
    let push_edges = |at: usize, stack: &mut Vec<usize>, buf: &mut Vec<Reg>| {
        let op = &code[at];
        kills(op, buf);
        let killed = buf.contains(&obj);
        let fall_kills = matches!(*op, Op::GetFieldOr { dst, .. } | Op::TakeFieldOr { dst, .. } if dst == obj);
        let only_failure = match (*op, tag) {
            (Op::TestTag { obj: o, name: m, .. } | Op::TestTagDrop { obj: o, name: m, .. }, Some(proven)) if o == obj => {
                name(m) != name(proven)
            }
            _ => false,
        };
        let failure = match *op {
            Op::GetFieldOr { to, .. } | Op::TakeFieldOr { to, .. } | Op::TestTag { to, .. } | Op::TestTagDrop { to, .. } => {
                Some(to as usize)
            }
            _ => None,
        };
        successors(op, at, code.len(), |s| {
            let is_failure = failure == Some(s) && s != at + 1;
            if killed || (fall_kills && !is_failure) || (only_failure && !is_failure) {
                return;
            }
            stack.push(s);
        });
    };
    push_edges(ip, &mut stack, &mut buf);
    let mut budget = 20_000usize;
    while let Some(at) = stack.pop() {
        if std::mem::replace(&mut seen[at], true) {
            continue;
        }
        budget = match budget.checked_sub(1) {
            Some(b) => b,
            None => return false,
        };
        let conflict = match code[at] {
            Op::GetField { obj: o, name: n, .. }
            | Op::GetFieldOr { obj: o, name: n, .. }
            | Op::TakeField { obj: o, name: n, .. }
            | Op::TakeFieldOr { obj: o, name: n, .. }
            | Op::GetOptField { obj: o, name: n, .. }
                if o == obj =>
            {
                !matches!(part, Component::Field(f) if Some(f) != name(n))
            }
            Op::GetPayload { obj: o, i, .. } | Op::TakePayload { obj: o, i, .. } if o == obj => {
                !matches!(part, Component::Payload(p) if p != i)
            }
            Op::GetIndex { obj: o, i, .. } | Op::TakeIndex { obj: o, i, .. } if o == obj => {
                !matches!(part, Component::Index(p) if p != i)
            }
            Op::GetRest { obj: o, name: n, n: count, .. } if o == obj => match part {
                Component::Field(f) => !(n..n.saturating_add(count)).any(|k| name(k) == Some(f)),
                Component::Payload(_) | Component::Index(_) => true,
            },
            // A record update copies every field except the ones it replaces, so a
            // field it replaces may already have been moved out. The new values are
            // read too, and one of them may be the whole record.
            Op::UpdateRecord { obj: o, name: n, n: count, base, .. } if o == obj => {
                (base..base.saturating_add(count)).contains(&obj)
                    || match part {
                        Component::Field(f) => !(n..n.saturating_add(count)).any(|k| name(k) == Some(f)),
                        Component::Payload(_) | Component::Index(_) => true,
                    }
            }
            Op::TestTag { obj: o, .. }
            | Op::TestTagDrop { obj: o, .. }
            | Op::TestRecord { obj: o, .. }
            | Op::TestTuple { obj: o, .. }
            | Op::NoMatch { obj: o }
                if o == obj =>
            {
                false
            }
            ref op => {
                reads(op, &mut buf);
                buf.contains(&obj)
            }
        };
        if conflict {
            return false;
        }
        push_edges(at, &mut stack, &mut buf);
    }
    true
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
            // `GetFieldOr` writes `dst` on its fall-through edge only, so `dst` is dead
            // coming in on that edge and alive on the jump only if the target reads it.
            // `kills` cannot say "on one edge", so it is done here.
            if let Op::GetFieldOr { dst, to, .. } | Op::TakeFieldOr { dst, to, .. } = code[ip] {
                scratch.fill(0);
                if ip + 1 < code.len() {
                    for (o, w) in scratch.iter_mut().zip(&live[(ip + 1) * words..(ip + 2) * words]) {
                        *o |= *w;
                    }
                }
                clear(&mut scratch, dst);
                let to = to as usize;
                if to < code.len() {
                    for (o, w) in scratch.iter_mut().zip(&live[to * words..(to + 1) * words]) {
                        *o |= *w;
                    }
                }
            }
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
        | Op::JumpFalseDrop { to, .. }
        | Op::TestTagDrop { to, .. }
        | Op::TakeFieldOr { to, .. }
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
        | Op::TakeField { .. }
        | Op::GetOptField { .. }
        | Op::GetIndex { .. }
        | Op::TakeIndex { .. }
        | Op::GetPayload { .. }
        | Op::TakePayload { .. }
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
        | Op::JumpFalseDrop { cond, .. }
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
        | Op::TakeField { obj, .. }
        | Op::GetOptField { obj, .. }
        | Op::GetIndex { obj, .. }
        | Op::TakeIndex { obj, .. }
        | Op::GetPayload { obj, .. }
        | Op::TakePayload { obj, .. }
        | Op::GetFieldOr { obj, .. }
        | Op::TakeFieldOr { obj, .. }
        | Op::TestTagDrop { obj, .. }
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
        | Op::TakeField { dst, .. }
        | Op::GetOptField { dst, .. }
        | Op::GetIndex { dst, .. }
        | Op::TakeIndex { dst, .. }
        | Op::GetPayload { dst, .. }
        | Op::TakePayload { dst, .. }
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
        | Op::JumpFalseDrop { .. }
        | Op::Ret { .. }
        | Op::ListPush { .. }
        | Op::TestLit { .. }
        | Op::TestLitDyn { .. }
        | Op::TestStr { .. }
        | Op::TestTag { .. }
        | Op::TestTagDrop { .. }
        | Op::TestTuple { .. }
        | Op::TestRecord { .. }
        | Op::TestList { .. }
        | Op::TestBool { .. }
        | Op::NoMatch { .. }
        | Op::GetFieldOr { .. }
        | Op::TakeFieldOr { .. }
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
        mark_takes(&mut code, 8, &[]);
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
        mark_takes(&mut code, 8, &[]);
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
        mark_takes(&mut code, 8, &[]);
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
        mark_takes(&mut code, 4, &[]);
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
        mark_takes(&mut code, 8, &[]);
        assert!(matches!(code[1], Op::UpdateRecord { take: true, .. }), "{:?}", code[1]);
    }

    /// `r.xs` and then `r.n`: the record stays live, but nothing reads `xs` again, so
    /// that read may move `xs` out. The later `n` read is a different part.
    #[test]
    fn a_field_nothing_reads_again_is_taken_while_the_record_lives() {
        let mut code = vec![
            Op::GetField { dst: 2, obj: 1, name: 0 },
            Op::GetField { dst: 3, obj: 1, name: 1 },
            Op::Ret { src: 2 },
        ];
        mark_takes(&mut code, 8, &["xs", "n"]);
        assert!(matches!(code[0], Op::TakeField { .. }), "{:?}", code[0]);
        assert!(matches!(code[1], Op::TakeField { .. }), "{:?}", code[1]);
    }

    /// The same field read twice: the first read must leave it.
    #[test]
    fn a_field_read_again_is_not_taken() {
        let mut code = vec![
            Op::GetField { dst: 2, obj: 1, name: 0 },
            Op::GetField { dst: 3, obj: 1, name: 0 },
            Op::Ret { src: 2 },
        ];
        mark_takes(&mut code, 8, &["xs"]);
        assert!(matches!(code[0], Op::GetField { .. }), "{:?}", code[0]);
    }

    /// A field read and then the whole record used: moving the field out would show.
    #[test]
    fn a_field_of_a_record_used_whole_later_is_not_taken() {
        let mut code = vec![
            Op::GetField { dst: 2, obj: 1, name: 0 },
            Op::Ret { src: 1 },
        ];
        mark_takes(&mut code, 8, &["xs"]);
        assert!(matches!(code[0], Op::GetField { .. }), "{:?}", code[0]);
    }

    /// A match: the payload read under `Found` may be taken even though the failure
    /// edge reaches the `Missing` arm's reads of the same value, because a value that
    /// passed `Found` cannot pass `Missing`. Only `Missing`'s own payload read counts
    /// then, and it is a DIFFERENT tag's.
    #[test]
    fn a_payload_under_a_proven_tag_is_taken_past_the_other_arm() {
        let mut code = vec![
            Op::TestTag { obj: 0, name: 0, n: 1, to: 4 },  // 0: Found?
            Op::GetPayload { dst: 1, obj: 0, i: 0 },       // 1
            Op::TestRecord { obj: 1, to: 4 },                           // 2: may fail on
            Op::Ret { src: 1 },                                         // 3
            Op::TestTag { obj: 0, name: 1, n: 1, to: 7 },  // 4: Missing?
            Op::GetPayload { dst: 2, obj: 0, i: 0 },       // 5
            Op::Ret { src: 2 },                                         // 6
            Op::NoMatch { obj: 0 },                                     // 7
        ];
        mark_takes(&mut code, 8, &["Found", "Missing"]);
        assert!(matches!(code[1], Op::TakePayload { .. }), "{:?}", code[1]);
    }

    /// Without the proven tag (the read is reachable by a jump), the other arm's
    /// payload read of the same element counts, and the take is refused.
    #[test]
    fn a_payload_reachable_by_a_jump_is_not_taken_past_another_read() {
        let mut code = vec![
            Op::Jump { to: 1 },                                         // 0
            Op::GetPayload { dst: 1, obj: 0, i: 0 },       // 1: jumped to
            Op::GetPayload { dst: 2, obj: 0, i: 0 },       // 2
            Op::Ret { src: 2 },
        ];
        mark_takes(&mut code, 8, &[]);
        assert!(matches!(code[1], Op::GetPayload { .. }), "{:?}", code[1]);
    }

    /// Round a loop the same field is read again: not taken.
    #[test]
    fn a_field_read_again_round_a_loop_is_not_taken() {
        let mut code = vec![
            Op::GetField { dst: 2, obj: 1, name: 0 },     // 0
            Op::Jump { to: 0 },                                         // 1
        ];
        mark_takes(&mut code, 8, &["xs"]);
        assert!(matches!(code[0], Op::GetField { .. }), "{:?}", code[0]);
    }

    /// A branch where one arm returns the scrutinee and the other does not: leaving by
    /// the other arm clears it, so it stops sharing what that arm is about to change.
    #[test]
    fn a_value_only_one_arm_needs_is_cleared_on_the_other() {
        let mut code = vec![
            Op::TestTag { obj: 1, name: 0, n: 0, to: 2 },  // 0
            Op::Ret { src: 0 },                                         // 1: needs reg 0
            Op::Ret { src: 1 },                                         // 2: does not
        ];
        let drops = mark_takes(&mut code, 4, &["Found"]);
        assert!(matches!(code[0], Op::TestTagDrop { drop: 1, .. }), "{:?}", code[0]);
        // And symmetrically: falling through, register 1 is the one only the jump
        // target needs.
        assert_eq!(drops, vec![(vec![1], vec![0])]);
    }

    /// The same, with the original still wanted afterwards.
    #[test]
    fn an_updated_record_still_in_use_is_not_taken() {
        let mut code = vec![
            Op::UpdateRecord { dst: 1, obj: 0, name: 0, base: 2, n: 1, take: false },
            Op::Ret { src: 0 },
        ];
        mark_takes(&mut code, 4, &[]);
        assert!(matches!(code[0], Op::UpdateRecord { take: false, .. }), "{:?}", code[0]);
    }

    /// `{ ..r, xs: r.xs.append(x) }`: the update copies every field except `xs`, so
    /// the `xs` read before it is the last one and may move the list out.
    #[test]
    fn a_field_the_update_replaces_is_taken() {
        let mut code = vec![
            Op::GetField { dst: 3, obj: 1, name: 0 },
            Op::UpdateRecord { dst: 2, obj: 1, name: 1, base: 3, n: 1, take: false },
            Op::Ret { src: 2 },
        ];
        mark_takes(&mut code, 8, &["xs", "xs"]);
        assert!(matches!(code[0], Op::TakeField { .. }), "{:?}", code[0]);
    }

    /// The update keeps `xs` when it replaces only `n`, so `xs` is read again.
    #[test]
    fn a_field_the_update_keeps_is_not_taken() {
        let mut code = vec![
            Op::GetField { dst: 3, obj: 1, name: 0 },
            Op::UpdateRecord { dst: 2, obj: 1, name: 1, base: 3, n: 1, take: false },
            Op::Ret { src: 2 },
        ];
        mark_takes(&mut code, 8, &["xs", "n"]);
        assert!(matches!(code[0], Op::GetField { .. }), "{:?}", code[0]);
    }

    /// `{ ..r, xs: r.xs.append(x), n: List.len(r.xs) }`: read twice, so neither read
    /// takes, even though the update itself replaces `xs`.
    #[test]
    fn a_replaced_field_read_twice_is_not_taken() {
        let mut code = vec![
            Op::GetField { dst: 3, obj: 1, name: 0 },
            Op::GetField { dst: 4, obj: 1, name: 0 },
            Op::UpdateRecord { dst: 2, obj: 1, name: 1, base: 3, n: 2, take: false },
            Op::Ret { src: 2 },
        ];
        mark_takes(&mut code, 8, &["xs", "xs", "n"]);
        assert!(matches!(code[0], Op::GetField { .. }), "{:?}", code[0]);
    }

    /// `$r = { ..$r, .. }` round a loop: the update writes over its own source, so
    /// the old record is dead there even though the register is read again.
    #[test]
    fn an_update_written_back_over_its_source_is_taken() {
        let mut code = vec![
            Op::UpdateRecord { dst: 1, obj: 1, name: 0, base: 2, n: 1, take: false },
            Op::Jump { to: 0 },
        ];
        mark_takes(&mut code, 4, &["xs"]);
        assert!(matches!(code[0], Op::UpdateRecord { take: true, .. }), "{:?}", code[0]);
    }

    /// `(t.0.append(x), t.1 + 1)`: a different element is read next, so `t.0` moves.
    #[test]
    fn a_tuple_element_nothing_reads_again_is_taken() {
        let mut code = vec![
            Op::GetIndex { dst: 2, obj: 1, i: 0 },
            Op::GetIndex { dst: 3, obj: 1, i: 1 },
            Op::Ret { src: 1 },
        ];
        mark_takes(&mut code, 8, &[]);
        assert!(matches!(code[0], Op::GetIndex { .. }), "{:?}", code[0]);
        let mut code = vec![
            Op::GetIndex { dst: 2, obj: 1, i: 0 },
            Op::GetIndex { dst: 3, obj: 1, i: 1 },
            Op::MakeTuple { dst: 1, base: 2, n: 2 },
            Op::Ret { src: 1 },
        ];
        mark_takes(&mut code, 8, &[]);
        assert!(matches!(code[0], Op::TakeIndex { i: 0, .. }), "{:?}", code[0]);
        assert!(matches!(code[1], Op::TakeIndex { i: 1, .. }), "{:?}", code[1]);
    }

    /// The same element read twice: the first read must leave it.
    #[test]
    fn a_tuple_element_read_again_is_not_taken() {
        let mut code = vec![
            Op::GetIndex { dst: 2, obj: 1, i: 0 },
            Op::GetIndex { dst: 3, obj: 1, i: 0 },
            Op::MakeTuple { dst: 1, base: 2, n: 2 },
            Op::Ret { src: 1 },
        ];
        mark_takes(&mut code, 8, &[]);
        assert!(matches!(code[0], Op::GetIndex { .. }), "{:?}", code[0]);
        assert!(matches!(code[1], Op::TakeIndex { .. }), "{:?}", code[1]);
    }
}
