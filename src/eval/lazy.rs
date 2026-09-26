//! A lazy iterator, the value `Iter.custom` builds and the iterator methods return.
//!
//! roc's `Iter` is a `{ len_if_known, step }` record whose `step` closure returns
//! `One({item, rest})`, `Skip({rest})` or `Done`. rocflight keeps the eager path for a
//! `List` receiver and an integer `Range` — the benchmark iteration cost lives there —
//! and represents everything that must observe laziness (a filtered source's unknown
//! length, a `Skip`, an infinite `custom`, a `Dec` or float range, a range walked
//! without a list) as this value instead. Each step is pure: it hands back the item
//! and the REST iterator, never mutating in place, exactly as roc's does.

use super::{apply_binop, call_function, EvalError, Value};
use crate::ast::BinOp;
use std::rc::Rc;

/// A length that may not be known ahead of time — roc's `[Known(U64), Unknown]`.
#[derive(Clone, Copy)]
pub enum Hint {
    Known(u64),
    Unknown,
}

impl Hint {
    pub fn value(self) -> Value {
        match self {
            Hint::Known(n) => Value::tag("Known", [Value::Int(n as i128)]),
            Hint::Unknown => Value::bare("Unknown"),
        }
    }
}

/// One step of an iterator: nothing left, an item skipped, or an item and the rest.
pub enum Step {
    Done,
    Skip(Rc<Lazy>),
    One(Value, Rc<Lazy>),
}

#[derive(Clone)]
pub enum Lazy {
    /// A list walked without copying it.
    List(Rc<Vec<Value>>, usize),
    /// An ascending numeric range, `step > 0`, over any numeric type — the elements
    /// are `Value`s so `Dec` and floats work through the same arithmetic as integers.
    Range { at: Value, end: Value, step: Value, inclusive: bool },
    /// The general unfold: `adv(state)` gives `Ok((item, state))` or `Err(NoMore)`.
    Custom { state: Value, adv: Value, hint: Hint },
    /// `keep_if` (keep = true) or `drop_if` (keep = false): a rejected item is a Skip.
    Filter { inner: Rc<Lazy>, pred: Value, keep: bool },
    Map { inner: Rc<Lazy>, f: Value },
    WithIndex { inner: Rc<Lazy>, i: u64 },
    /// Yield one element, then skip `step - 1`; `skip` counts down to the next emit.
    StepBy { inner: Rc<Lazy>, step: u64, skip: u64 },
    Concat { first: Rc<Lazy>, second: Rc<Lazy> },
    Take { inner: Rc<Lazy>, n: u64 },
    /// `drop_first`: each of the first `n` items is a Skip, as in roc's.
    Drop { inner: Rc<Lazy>, n: u64 },
}

fn rc(l: Lazy) -> Rc<Lazy> {
    Rc::new(l)
}

/// An iterator with nothing left in it.
///
/// A walked-out list is exactly that, so this is the shape every exhausted iterator
/// takes: a range that cannot advance without overflowing its width becomes one, and so
/// does the register a `for` loop leaves behind when it finishes.
pub fn exhausted() -> Rc<Lazy> {
    rc(Lazy::List(Rc::new(Vec::new()), 0))
}

/// Any value seen as a lazy iterator: a list or range is wrapped, an `Iter` is itself.
pub fn of(value: Value) -> Option<Rc<Lazy>> {
    match value {
        Value::Iter(l) => Some(l),
        Value::List(items) => Some(rc(Lazy::List(items, 0))),
        Value::Range { start, end, inclusive, step } => Some(rc(Lazy::Range {
            at: Value::Int(start),
            end: Value::Int(end),
            step: Value::Int(i128::from(step)),
            inclusive,
        })),
        _ => None,
    }
}

fn truthy(v: Value) -> bool {
    matches!(v, Value::Bool(true))
}

/// Is this number below zero?
///
/// `Lazy::Range` asks it of its step on EVERY element, and it used to ask through
/// `apply_binop` twice — a subtraction to make a zero of the step's own kind, because
/// `Dec` against `Int` is not a comparison `apply_binop` makes, and then a `<` against
/// it. Two full operator dispatches per element to re-answer a property of a value that
/// does not change while the range is walked. The kinds are exactly the ones a range's
/// step can be, and anything else keeps the old answer of `false`.
fn negative(step: &Value) -> bool {
    match step {
        Value::Int(n) | Value::Dec(n) => *n < 0,
        Value::Float(f) => *f < 0.0,
        Value::F32(f) => *f < 0.0,
        _ => false,
    }
}

/// What one step produced, before it is paired with the iterator that remains.
///
/// The same three cases as `Step` with neither payload: `advance` works on a `&mut Lazy`,
/// so the iterator that remains IS the one it advanced, and the item goes through an out
/// parameter. Both omissions are for size — a `Value` is 48 bytes and an `EvalError`
/// carries a `String`, so returning them from each layer of a chain moved 80 bytes per
/// layer per element. Measured: with the item in here, a plain range cost 7% more than
/// not having this function at all.
enum Made {
    Done,
    Skip,
    One,
}

impl Lazy {
    /// One step: the item, and the iterator that remains.
    ///
    /// Takes the `Rc` by VALUE and advances through `Rc::make_mut`, so an iterator
    /// nothing else holds is stepped **in place** — the whole chain of it, because
    /// `advance` recurses on `&mut Lazy` rather than on `Rc<Lazy>`. Walking n elements
    /// of a `map` over a `filter` over a list allocates nothing; it used to allocate a
    /// fresh rest per element PER LAYER, three deep for that chain.
    ///
    /// `make_mut` rather than `get_mut` is what keeps this invisible. A caller that kept
    /// its own handle — `Iter.next` in Roc, which must not mutate the iterator the
    /// program is holding — gets a copy to advance and leaves the original alone, which
    /// is exactly the behaviour the old per-step allocation gave it.
    pub fn step(mut self: Rc<Self>) -> Result<Step, EvalError> {
        let mut item = Value::Unit;
        Ok(match Rc::make_mut(&mut self).advance(&mut item)? {
            Made::Done => Step::Done,
            Made::Skip => Step::Skip(self),
            Made::One => Step::One(item, self),
        })
    }

    /// Advance this iterator by one, in place, and say what came out.
    ///
    /// A wrapper advances its own inner the same way, so the recursion carries the
    /// mutable borrow down the chain instead of rebuilding it on the way back up.
    ///
    /// Split from `advance_wrapped` so that THIS half — the two iterators with no inner,
    /// which is what a plain loop walks — carries no recursion and can be inlined into
    /// `step`. Measured: with the leaves and the wrappers in one recursive function,
    /// LLVM keeps the whole thing out of line and a plain range costs 10% more.
    #[inline]
    fn advance(&mut self, out: &mut Value) -> Result<Made, EvalError> {
        match self {
            Lazy::List(items, at) => Ok(match items.get(*at) {
                Some(item) => {
                    *out = item.clone();
                    *at += 1;
                    Made::One
                }
                None => Made::Done,
            }),
            Lazy::Range { at, end, step: by, inclusive } => {
                // A negative step walks DOWN to `end`: a reversed range
                // (`5.range_exclusive_from(1)`) is one.
                let descending = negative(by);
                let op = match (*inclusive, descending) {
                    (true, false) => BinOp::Le,
                    (false, false) => BinOp::Lt,
                    (true, true) => BinOp::Ge,
                    (false, true) => BinOp::Gt,
                };
                if !truthy(apply_binop(op, at, end)?) {
                    return Ok(Made::Done);
                }
                *out = at.clone();
                // Advancing past the last element can overflow the width (an inclusive
                // range ending at the maximum). That must stop the iterator rather than
                // crash, and a step that wraps without advancing is the same case, so
                // both become an iterator with nothing left in it.
                let past = if descending { BinOp::Lt } else { BinOp::Gt };
                match apply_binop(BinOp::Add, at, by) {
                    Ok(next) if apply_binop(past, &next, at).map(truthy).unwrap_or(false) => {
                        *at = next;
                    }
                    _ => *self = Lazy::List(Rc::new(Vec::new()), 0),
                }
                Ok(Made::One)
            }
            _ => self.advance_wrapped(out),
        }
    }

    /// The wrapping iterators. Each advances its own inner IN PLACE — `Rc::make_mut`
    /// hands it a `&mut Lazy` to recurse into — rather than stepping a clone and
    /// rebuilding itself around the rest. A `map` over a `filter` over a list used to be
    /// three allocations per element and is now none.
    fn advance_wrapped(&mut self, out: &mut Value) -> Result<Made, EvalError> {
        match self {
            // `advance` answers these itself and never comes here; handing them back
            // terminates, because that path does not call this one.
            Lazy::List(..) | Lazy::Range { .. } => self.advance(out),
            Lazy::Custom { state, adv, hint } => {
                match call_function(adv.clone(), vec![state.clone()])? {
                    Value::Tag("Ok", payload) => match payload.first() {
                        Some(Value::Tuple(pair)) if pair.len() == 2 => {
                            *hint = Hint::step_down(*hint);
                            *state = pair[1].clone();
                            *out = pair[0].clone();
                            Ok(Made::One)
                        }
                        other => Err(EvalError {
                            message: format!("Iter.custom step must give Ok((item, state)), got {:?}", other),
                        }),
                    },
                    _ => Ok(Made::Done),
                }
            }
            Lazy::Filter { inner, pred, keep } => match Rc::make_mut(inner).advance(out)? {
                Made::Done => Ok(Made::Done),
                Made::Skip => Ok(Made::Skip),
                Made::One => {
                    // A rejected item is a Skip, not an absence: roc's `Iter` reports
                    // one so that a caller stepping by hand sees the work happen.
                    if truthy(call_function(pred.clone(), vec![out.clone()])?) == *keep {
                        Ok(Made::One)
                    } else {
                        Ok(Made::Skip)
                    }
                }
            },
            Lazy::Map { inner, f } => match Rc::make_mut(inner).advance(out)? {
                Made::Done => Ok(Made::Done),
                Made::Skip => Ok(Made::Skip),
                Made::One => {
                    let item = std::mem::replace(out, Value::Unit);
                    *out = call_function(f.clone(), vec![item])?;
                    Ok(Made::One)
                }
            },
            Lazy::WithIndex { inner, i } => match Rc::make_mut(inner).advance(out)? {
                Made::Done => Ok(Made::Done),
                Made::Skip => Ok(Made::Skip),
                Made::One => {
                    let item = std::mem::replace(out, Value::Unit);
                    *out = Value::tuple(vec![Value::Int(i128::from(*i)), item]);
                    *i += 1;
                    Ok(Made::One)
                }
            },
            Lazy::StepBy { inner, step, skip } => match Rc::make_mut(inner).advance(out)? {
                Made::Done => Ok(Made::Done),
                Made::Skip => Ok(Made::Skip),
                Made::One => {
                    if *skip == 0 {
                        *skip = step.saturating_sub(1);
                        Ok(Made::One)
                    } else {
                        *skip -= 1;
                        Ok(Made::Skip)
                    }
                }
            },
            Lazy::Concat { first, second } => match Rc::make_mut(first).advance(out)? {
                // The first is spent, so this iterator simply IS the second from here
                // on — which is what returning `second.step()` did, one layer at a time.
                Made::Done => {
                    let rest = (**second).clone();
                    *self = rest;
                    self.advance(out)
                }
                Made::Skip => Ok(Made::Skip),
                Made::One => Ok(Made::One),
            },
            Lazy::Drop { inner, n } => match Rc::make_mut(inner).advance(out)? {
                Made::One if *n > 0 => {
                    *n -= 1;
                    Ok(Made::Skip)
                }
                made => Ok(made),
            },
            Lazy::Take { inner, n } => {
                if *n == 0 {
                    return Ok(Made::Done);
                }
                match Rc::make_mut(inner).advance(out)? {
                    Made::Done => Ok(Made::Done),
                    Made::Skip => Ok(Made::Skip),
                    Made::One => {
                        *n -= 1;
                        Ok(Made::One)
                    }
                }
            }
        }
    }

    pub fn hint(&self) -> Hint {
        match self {
            Lazy::List(items, at) => Hint::Known((items.len() - at.min(&items.len())) as u64),
            Lazy::Range { at, end, step, inclusive } => range_count(at, end, step, *inclusive),
            Lazy::Custom { hint, .. } => *hint,
            // Filtering cannot know how many survive.
            Lazy::Filter { .. } => Hint::Unknown,
            Lazy::Map { inner, .. } | Lazy::WithIndex { inner, .. } => inner.hint(),
            Lazy::StepBy { inner, step, .. } => match inner.hint() {
                Hint::Known(n) => Hint::Known(n.div_ceil((*step).max(1))),
                Hint::Unknown => Hint::Unknown,
            },
            Lazy::Concat { first, second } => match (first.hint(), second.hint()) {
                (Hint::Known(a), Hint::Known(b)) => Hint::Known(a.saturating_add(b)),
                _ => Hint::Unknown,
            },
            Lazy::Drop { inner, n } => match inner.hint() {
                Hint::Known(m) => Hint::Known(m.saturating_sub(*n)),
                Hint::Unknown => Hint::Unknown,
            },
            Lazy::Take { inner, n } => match inner.hint() {
                Hint::Known(m) => Hint::Known(m.min(*n)),
                Hint::Unknown => Hint::Unknown,
            },
        }
    }
}

impl Hint {
    fn step_down(self) -> Hint {
        match self {
            Hint::Known(n) => Hint::Known(n.saturating_sub(1)),
            Hint::Unknown => Hint::Unknown,
        }
    }
}

/// The number of elements in an ascending numeric range, or `Unknown` when it does not
/// fit a `U64` or the bounds are not the kind this counts (a float range is walked, not
/// counted).
pub fn range_count(at: &Value, end: &Value, step: &Value, inclusive: bool) -> Hint {
    // A `U128` range counts in `u128`: `0..<U128.highest` has more elements than a
    // `U64` can hold, which is `Unknown`.
    if let (Value::U128(a), Value::U128(e)) = (at, end) {
        let s = match step { Value::U128(s) => *s, Value::Int(s) => *s as u128, _ => 1 };
        if s == 0 { return Hint::Known(0); }
        let last = if inclusive { *e } else { e.wrapping_sub(1) };
        if last < *a { return Hint::Known(0); }
        return match u64::try_from((last - a) / s + 1) { Ok(n) => Hint::Known(n), Err(_) => Hint::Unknown };
    }
    let whole = |v: &Value| match v {
        Value::Int(n) => Some(*n),
        Value::Dec(n) => Some(*n),
        _ => None,
    };
    let (Some(a), Some(e), Some(s)) = (whole(at), whole(end), whole(step)) else {
        return Hint::Unknown;
    };
    if s <= 0 {
        return Hint::Known(0);
    }
    let last = if inclusive { e } else { e - 1 };
    if last < a {
        return Hint::Known(0);
    }
    match u128::try_from(last - a).ok().map(|d| d / s as u128 + 1).and_then(|n| u64::try_from(n).ok()) {
        Some(n) => Hint::Known(n),
        None => Hint::Unknown,
    }
}

/// `Iter.custom(state, len_if_known, adv)`.
pub fn custom(state: Value, hint: &Value, adv: Value) -> Value {
    let hint = match hint {
        Value::Tag("Known", payload) => match payload.first() {
            Some(Value::Int(n)) => Hint::Known(*n as u64),
            _ => Hint::Unknown,
        },
        _ => Hint::Unknown,
    };
    Value::Iter(rc(Lazy::Custom { state, adv, hint }))
}

/// The rest iterator as a value: an `Iter`, so `Iter.next` keeps returning `One`/`Skip`
/// with a real rest even when the source was a list or range.
fn rest_value(rest: Rc<Lazy>) -> Value {
    Value::Iter(rest)
}

/// The iterator methods, dispatched here when the receiver is a `Value::Iter`. The lazy
/// transformers wrap; the terminal ones drive the steps. Returns `None` for a method
/// this does not handle, so the caller can fall back.
pub fn call(name: &str, args: &mut [Value]) -> Option<Result<Value, EvalError>> {
    let this = args.first().cloned()?;
    let iter = of(this)?;
    Some(match name {
        "iter" | "collect_iter" => Ok(Value::Iter(iter)),
        "keep_if" | "drop_if" => Ok(Value::Iter(rc(Lazy::Filter {
            inner: iter,
            pred: args.get(1).cloned().unwrap_or(Value::Unit),
            keep: name == "keep_if",
        }))),
        "map" => Ok(Value::Iter(rc(Lazy::Map { inner: iter, f: args.get(1).cloned().unwrap_or(Value::Unit) }))),
        "with_index" => Ok(Value::Iter(rc(Lazy::WithIndex { inner: iter, i: 0 }))),
        "step_by" => {
            let wanted = args.get(1).cloned().unwrap_or(Value::Unit);
            // A NUMERIC range keeps its bounds rather than counting elements, so the
            // step may be fractional and a descending range re-anchors; see `step_range`.
            if let Lazy::Range { .. } = &*iter {
                return Some(step_range(&iter, &wanted));
            }
            match super::as_index(&wanted) {
                Some(0) | None => Ok(Value::Iter(rc(Lazy::List(std::rc::Rc::new(Vec::new()), 0)))),
                Some(step) => Ok(Value::Iter(rc(Lazy::StepBy { inner: iter, step: step as u64, skip: 0 }))),
            }
        }
        "take_first" => match super::as_index(args.get(1).unwrap_or(&Value::Unit)) {
            Some(n) => Ok(Value::Iter(rc(Lazy::Take { inner: iter, n: n as u64 }))),
            None => Ok(Value::Iter(iter)),
        },
        "drop_first" => match super::as_index(args.get(1).unwrap_or(&Value::Unit)) {
            Some(n) => Ok(Value::Iter(rc(Lazy::Drop { inner: iter, n: n as u64 }))),
            None => Ok(Value::Iter(iter)),
        },
        // One item before or after: a concatenation with a one-item iterator.
        "prepended" | "append" => {
            let one = rc(Lazy::List(Rc::new(vec![args.get(1).cloned().unwrap_or(Value::Unit)]), 0));
            let (first, second) = if name == "prepended" { (one, iter) } else { (iter, one) };
            Ok(Value::Iter(rc(Lazy::Concat { first, second })))
        }
        // Terminal, so every item is walked anyway: the List answer over them.
        "min" | "max" => materialize(&iter).map(|items| super::extreme(name, items.into_iter(), "IterWasEmpty")),
        "concat" => match args.get(1).cloned().and_then(of) {
            Some(second) => Ok(Value::Iter(rc(Lazy::Concat { first: iter, second }))),
            None => Err(EvalError { message: "Iter.concat needs two iterators".to_string() }),
        },
        "size_hint" => Ok(iter.hint().value()),
        "next" => next(&iter),
        "fold" => {
            let init = args.get(1).cloned().unwrap_or(Value::Unit);
            let f = args.get(2).cloned().unwrap_or(Value::Unit);
            fold(&iter, init, f)
        }
        "collect" | "to_list" | "from_iter" => materialize(&iter).map(Value::list),
        "sum" => fold_native(&iter, Value::Int(0), |acc, x| apply_binop(BinOp::Add, &acc, &x)),
        // A `Try`, starting from the first item, as `Builtin.roc`'s is.
        "product" => materialize(&iter).and_then(|items| {
            let mut items = items.into_iter();
            let Some(first) = items.next() else {
                return Ok(Value::tag("Err", [Value::bare("IterWasEmpty")]));
            };
            let product = items.try_fold(first, |acc, x| apply_binop(BinOp::Mul, &acc, &x))?;
            Ok(Value::tag("Ok", [product]))
        }),
        "len" => materialize(&iter).map(|v| Value::Int(v.len() as i128)),
        _ => return None,
    })
}

/// `step_by` over a numeric range: the step is ABSOLUTE, so it replaces the range's own
/// rather than composing with it (roc's `(1..=10).step_by(2).step_by(3)` is
/// `[1, 4, 7, 10]`), and it may be fractional on a fractional range.
///
/// A DESCENDING range — what `5.range_inclusive_from(1)` builds — is anchored at its
/// LOWER bound, so the elements are the multiples of the step above that bound, walked
/// down: `5.Dec.range_inclusive_from(1).step_by(1.5)` is `[4.0, 2.5, 1.0]`.
pub fn step_range(iter: &Rc<Lazy>, wanted: &Value) -> Result<Value, EvalError> {
    let Lazy::Range { at, end, step, inclusive } = &**iter else {
        return Ok(Value::Iter(iter.clone()));
    };
    // Zeros of each value's own kind: `Dec` against `Int` is not a comparison
    // `apply_binop` makes.
    let zero = apply_binop(BinOp::Sub, wanted, wanted)?;
    if truthy(apply_binop(BinOp::Le, wanted, &zero)?) {
        return Ok(Value::list(Vec::new()));
    }
    let step_zero = apply_binop(BinOp::Sub, step, step)?;
    let descending = truthy(apply_binop(BinOp::Lt, step, &step_zero)?);
    let (at, step) = if descending {
        let mut anchor = end.clone();
        loop {
            let next = apply_binop(BinOp::Add, &anchor, wanted)?;
            if !truthy(apply_binop(BinOp::Le, &next, at)?) {
                break;
            }
            anchor = next;
        }
        (anchor, apply_binop(BinOp::Sub, &step_zero, wanted)?)
    } else {
        (at.clone(), wanted.clone())
    };
    Ok(Value::Iter(rc(Lazy::Range { at, end: end.clone(), step, inclusive: *inclusive })))
}

/// `Iter.next`: one step. roc's `next` reports `Skip` as it is — it does not skip
/// ahead — so a single step is the whole of it.
fn next(iter: &Rc<Lazy>) -> Result<Value, EvalError> {
    Ok(match Rc::clone(iter).step()? {
        Step::Done => Value::bare("Done"),
        Step::Skip(rest) => Value::tag("Skip", [Value::record(vec![("rest", rest_value(rest))])]),
        Step::One(item, rest) => {
            Value::tag("One", [Value::record(vec![("item", item), ("rest", rest_value(rest))])])
        }
    })
}

fn fold(iter: &Rc<Lazy>, init: Value, f: Value) -> Result<Value, EvalError> {
    fold_native(iter, init, |acc, item| call_function(f.clone(), vec![acc, item]))
}

fn fold_native(
    iter: &Rc<Lazy>,
    init: Value,
    mut f: impl FnMut(Value, Value) -> Result<Value, EvalError>,
) -> Result<Value, EvalError> {
    let mut acc = init;
    let mut current = iter.clone();
    loop {
        match current.step()? {
            Step::Done => return Ok(acc),
            Step::Skip(rest) => current = rest,
            Step::One(item, rest) => {
                acc = f(acc, item)?;
                current = rest;
            }
        }
    }
}

pub fn materialize(iter: &Rc<Lazy>) -> Result<Vec<Value>, EvalError> {
    let mut out = Vec::new();
    let mut current = iter.clone();
    loop {
        match current.step()? {
            Step::Done => return Ok(out),
            Step::Skip(rest) => current = rest,
            Step::One(item, rest) => {
                out.push(item);
                current = rest;
                if out.len() > 100_000_000 {
                    return Err(EvalError {
                        message: "iterator did not finish: collecting an unbounded source".to_string(),
                    });
                }
            }
        }
    }
}

/// `Str.iter_utf8`: the bytes, lazily, so `Iter.next` on it returns `One`/`Done`.
pub fn iter_utf8(text: &str) -> Value {
    Value::Iter(rc(Lazy::List(
        std::rc::Rc::new(text.bytes().map(|b| Value::Int(i128::from(b))).collect()),
        0,
    )))
}

