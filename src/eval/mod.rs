//! Roc's builtins, operators and runtime helpers.
//!
//! What is left of what used to be a tree-walking interpreter. The walker itself is
//! gone — the register VM in `crate::vm` runs every program now — and these are the
//! parts a compiled program still calls into: the forty-odd builtins, the operator
//! table, `Str.inspect`, and the handful of statement forms that do something to the
//! world (`expect`, `dbg`, `crash`, the host's `echo!`).
//!
//! They take and return `Value`s and hold no interpreter state, which is why they
//! survived the switch unchanged.

use crate::ast::{BinOp, Pattern};
use crate::error::EvalError;

pub mod crypto;
pub mod f32math;
pub mod lazy;
pub mod numeral;
pub mod value;

pub use value::{str_value, Value};


thread_local! {
    /// The `to_inspect` methods currently running, by name.
    ///
    /// A nominal's `to_inspect` almost always unwraps and inspects what is INSIDE —
    /// `|ItemKind.(k)| "ItemKind.(${Str.inspect(k)})"`. roc's unwrap changes the type,
    /// so the inner `Str.inspect` finds no custom method; rocflight erases nominals, so
    /// the inner value is the same value and the same method answers again, for ever.
    /// Refusing a method already on the stack is what ends it, and matches what roc
    /// does for the reason roc does it.
    static INSPECTING: std::cell::RefCell<Vec<&'static str>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// A nominal's own `to_inspect`, if it defines one.
fn custom_inspect(value: &Value) -> Option<Value> {
    for (name, func) in crate::vm::methods_named("to_inspect", value) {
        if INSPECTING.with(|running| running.borrow().contains(&name)) {
            continue;
        }
        INSPECTING.with(|running| running.borrow_mut().push(name));
        let shown = call_function(func, vec![value.clone()]);
        INSPECTING.with(|running| {
            running.borrow_mut().pop();
        });
        if let Ok(shown @ Value::Str(_)) = shown {
            return Some(shown);
        }
    }
    None
}


/// The methods every `Try` answers: `Ok(v)` and `Err(e)` are just tags, so these
/// cannot come from the builtin table, which is keyed by module.
///
/// `None` means the method is not one of these, and the caller carries on.
/// The `Try` methods — `Ok`/`Err` answer these whatever module they came from.
///
/// Takes VALUES: it always evaluated every argument up front anyway, and the VM's
/// dispatch opcode has nothing else to give it.
pub fn try_method(
    tag: &str,
    payload: &std::rc::Rc<[Value]>,
    method: &str,
    evaluated: Vec<Value>,
) -> Result<Option<Value>, EvalError> {
    let is_ok = tag == "Ok";
    let inner = payload.first().cloned().unwrap_or(Value::Unit);
    let first = || evaluated.first().cloned().unwrap_or(Value::Unit);

    Ok(Some(match method {
        "is_ok" => Value::Bool(is_ok),
        "is_err" => Value::Bool(!is_ok),
        // Rebuild the same tag around the mapped payload; the other side passes
        // through untouched.
        "map_ok" if is_ok => Value::tag("Ok", [call_function(first(), vec![inner])?]),
        "map_err" if !is_ok => Value::tag("Err", [call_function(first(), vec![inner])?]),
        // The payload is unchanged, so the same `Rc` serves: no copy, no allocation.
        "map_ok" | "map_err" => {
            Value::Tag(if is_ok { "Ok" } else { "Err" }, payload.clone())
        }
        "ok_or" => {
            if is_ok {
                inner
            } else {
                first()
            }
        }
        "on_err" if !is_ok => call_function(first(), vec![inner])?,
        "on_err" => Value::Tag("Ok", payload.clone()),
        _ => return Ok(None),
    }))
}

/// `List.*` builtins.
///
/// Argument order follows roc: the list comes first, and `fold` takes
/// `(list, initial, fn)` with the accumulator as the callback's first parameter.
/// `List.len` returns a U64 in roc — worth remembering, since mixing it with I64
/// arms in a match is a type error.
/// The elements of a List, or of a Range without ever building one.
///
/// `(1..=2_000_000).iter()` used to materialize two million `Value`s — 190 MB — before
/// the first element was looked at. A range knows its elements from three integers, so
/// the ones that only need to WALK the elements walk them instead.
enum Elements {
    /// Walked in place: the list is shared with whoever else holds it, and each
    /// element is cloned as it is reached rather than the whole list up front.
    List(std::rc::Rc<Vec<Value>>, usize),
    /// The next value, the last one, and the step between them.
    Range(i128, i128, i128),
}

impl Iterator for Elements {
    type Item = Value;

    fn next(&mut self) -> Option<Value> {
        match self {
            Elements::List(items, at) => {
                let item = items.get(*at).cloned();
                *at += 1;
                item
            }
            Elements::Range(next, last, step) => {
                if *step <= 0 || *next > *last {
                    return None;
                }
                let current = *next;
                // Past `i128::MAX` there is nothing left; a wrapped `next` would
                // never pass `last` again.
                match next.checked_add(*step) {
                    Some(n) => *next = n,
                    None => *step = 0,
                }
                Some(Value::Int(current))
            }
        }
    }
}

impl Elements {
    /// How many elements there are, without walking them.
    fn count_of(value: &Value) -> Option<usize> {
        match value {
            Value::List(items) => Some(items.len()),
            Value::Range { start, end, inclusive, step } => {
                let last = if *inclusive { *end } else { *end - 1 };
                if *step <= 0 || last < *start {
                    return Some(0);
                }
                Some(((last - start) / i128::from(*step) + 1) as usize)
            }
            _ => None,
        }
    }
}

/// A List or a Range, as something to iterate.
fn elements(value: Value, name: &str) -> Result<Elements, EvalError> {
    match value {
        Value::List(items) => Ok(Elements::List(items, 0)),
        Value::Range { start, end, inclusive, step } => {
            let last = if inclusive { end } else { end - 1 };
            Ok(Elements::Range(start, last, i128::from(step)))
        }
        other => Err(EvalError {
            message: format!("List.{} needs a List, got {}", name, other),
        }),
    }
}

fn call_list_builtin(name: &str, args: &mut [Value]) -> Result<Value, EvalError> {
    let expect = |wanted: usize, got: usize| -> Result<(), EvalError> {
        if wanted == got {
            Ok(())
        } else {
            Err(EvalError {
                message: format!("List.{} expects {} argument(s), got {}", name, wanted, got),
            })
        }
    };

    // Takes the argument OUT of `args` — which is the VM's own register window, so
    // there is no `Vec` here at all: when nothing else holds the list, the elements
    // come back without a copy and the operation below mutates them in place.
    let as_list = |v: &mut Value| -> Result<Vec<Value>, EvalError> {
        match std::mem::replace(v, Value::Unit) {
            Value::List(items) => Ok(value::into_items(items)),
            other => Err(EvalError {
                message: format!("List.{} needs a List, got {}", name, other),
            }),
        }
    };
    // A read leaves the list where it is: taking it would copy a shared one, and
    // `xs.get(i)` in a loop over `xs` was quadratic for exactly that reason.
    fn peek<'a>(v: &'a Value, name: &str) -> Result<&'a [Value], EvalError> {
        match v {
            Value::List(items) => Ok(items),
            other => Err(EvalError {
                message: format!("List.{} needs a List, got {}", name, other),
            }),
        }
    }

    // A range stays a range for the operations that only walk it; anything that
    // indexes, slices or sorts needs the list.
    if matches!(args.first(), Some(Value::Range { .. }))
        && !matches!(
            name,
            "len" | "is_empty" | "map" | "fold" | "keep_if" | "drop_if" | "fold_try" | "from_iter"
                | "contains" | "iter" | "any" | "all" | "sum" | "find_first" | "size_hint"
                | "fold_with_index" | "with_index" | "step_by" | "collect" | "count_if"
        )
    {
        let items: Vec<Value> = elements(args[0].clone(), name)?.collect();
        args[0] = Value::list(items);
    }

    match name {
        // `List.repeat(item, n)` — n copies. `Dict` allocates its bucket table this way.
        "repeat" => {
            expect(2, args.len())?;
            let count = as_index(&args[1]).ok_or_else(|| EvalError {
                message: format!("List.repeat needs a count, got {}", args[1]),
            })?;
            Ok(Value::list(vec![args[0].clone(); count]))
        }

        // Capacity is observable: roc's tests read it back after `with_capacity`,
        // `reserve` and `release_excess_capacity`, and a `Vec` answers the same way.
        "reserve" => {
            expect(2, args.len())?;
            let mut items = as_list(&mut args[0])?;
            if let Some(n) = as_index(&args[1]) {
                items.reserve(n);
            }
            Ok(Value::list(items))
        }
        "release_excess_capacity" => {
            expect(1, args.len())?;
            let mut items = as_list(&mut args[0])?;
            items.shrink_to_fit();
            Ok(Value::list(items))
        }
        "with_capacity" => {
            expect(1, args.len())?;
            Ok(Value::list(Vec::with_capacity(as_index(&args[0]).unwrap_or(0))))
        }
        // A list of `{}` takes no memory in roc, so its capacity is always zero.
        "capacity" => {
            expect(1, args.len())?;
            let items = peek(&args[0], name)?;
            let zero_sized = !items.is_empty() && items.iter().all(|v| matches!(v, Value::Unit));
            let capacity = if zero_sized { 0 } else if let Value::List(rc) = &args[0] { rc.capacity() } else { 0 };
            Ok(Value::Int(capacity as i128))
        }
        // `.iter()` and `.collect()` are the identity on what is already a list.
        "iter" | "collect" => {
            expect(1, args.len())?;
            Ok(std::mem::replace(&mut args[0], Value::Unit))
        }
        "iter_rev" => {
            expect(1, args.len())?;
            let mut items = as_list(&mut args[0])?;
            items.reverse();
            Ok(Value::list(items))
        }
        "size_hint" => {
            expect(1, args.len())?;
            let count = Elements::count_of(&args[0]).ok_or_else(|| EvalError {
                message: format!("List.{} needs a List, got {}", name, args[0]),
            })?;
            Ok(Value::tag("Known", [Value::Int(count as i128)]))
        }
        // `step_by(0)` yields nothing. On a range the step is ABSOLUTE — roc's
        // `(1..=10).step_by(2).step_by(3)` is `[1, 4, 7, 10]` — so it stays a range.
        "step_by" => {
            expect(2, args.len())?;
            // A lazy numeric range keeps its bounds rather than counting elements.
            if let Value::Iter(lazy) = &args[0] {
                if matches!(&**lazy, lazy::Lazy::Range { .. }) {
                    return lazy::step_range(lazy, &args[1]);
                }
            }
            let step = as_index(&args[1]).ok_or_else(|| EvalError {
                message: format!("List.step_by needs a count, got {}", args[1]),
            })?;
            if step == 0 {
                return Ok(Value::list(Vec::new()));
            }
            if let Value::Range { start, end, inclusive, .. } = args[0] {
                return Ok(Value::Range { start, end, inclusive, step: step as i64 });
            }
            Ok(Value::list(elements(args[0].clone(), name)?.step_by(step).collect()))
        }
        "single" => {
            expect(1, args.len())?;
            Ok(Value::list(vec![std::mem::replace(&mut args[0], Value::Unit)]))
        }
        "prepended" => {
            expect(2, args.len())?;
            let mut items = vec![args[1].clone()];
            items.extend(as_list(&mut args[0])?);
            Ok(Value::list(items))
        }
        "next" => {
            expect(1, args.len())?;
            let mut items = as_list(&mut args[0])?;
            Ok(if items.is_empty() {
                Value::bare("Done")
            } else {
                let item = items.remove(0);
                Value::tag("One", [Value::record(vec![("item", item), ("rest", Value::list(items))])])
            })
        }
        // `Iter.custom(state, hint, step)`: run the step until it says `NoMore`. Eager,
        // so an iterator that never ends is refused rather than run forever.
        // `Iter.custom(state, len_if_known, step)` builds a LAZY iterator: an
        // unbounded source (a `fib` unfold under `take_first`) never runs to a list.
        "custom" => {
            expect(3, args.len())?;
            Ok(lazy::custom(args[0].clone(), &args[1], args[2].clone()))
        }
        "with_index" => {
            expect(1, args.len())?;
            let items = elements(args[0].clone(), name)?;
            Ok(Value::list(
                items.enumerate().map(|(i, v)| Value::tuple(vec![Value::Int(i as i128), v])).collect(),
            ))
        }
        "fold_with_index" => {
            expect(3, args.len())?;
            let items = elements(args[0].clone(), name)?;
            let mut acc = args[1].clone();
            let func = args[2].clone();
            for (i, item) in items.enumerate() {
                acc = call_function(func.clone(), vec![acc, item, Value::Int(i as i128)])?;
            }
            Ok(acc)
        }
        "any" | "all" => {
            expect(2, args.len())?;
            let items = elements(args[0].clone(), name)?;
            let func = args[1].clone();
            let want = name == "any";
            for item in items {
                if matches!(call_function(func.clone(), vec![item])?, Value::Bool(b) if b == want) {
                    return Ok(Value::Bool(want));
                }
            }
            Ok(Value::Bool(!want))
        }
        // Not previously implemented at all — `List.count_if` was an unknown function,
        // and `Builtin.roc` declares it at `List(a), (a -> Bool) -> U64`. It is here so
        // that the compiled loop in `Compiler::list_loop` is an optimization and not the
        // only way to reach it.
        "count_if" => {
            expect(2, args.len())?;
            let items = elements(args[0].clone(), name)?;
            let func = args[1].clone();
            let mut count = 0i128;
            for item in items {
                if matches!(call_function(func.clone(), vec![item])?, Value::Bool(true)) {
                    count += 1;
                }
            }
            Ok(Value::Int(count))
        }
        "find_first" => {
            expect(2, args.len())?;
            let items = elements(args[0].clone(), name)?;
            let func = args[1].clone();
            for item in items {
                if matches!(call_function(func.clone(), vec![item.clone()])?, Value::Bool(true)) {
                    return Ok(Value::tag("Ok", [item]));
                }
            }
            Ok(Value::tag("Err", [Value::bare("NotFound")]))
        }
        "find_last" => {
            expect(2, args.len())?;
            let items: Vec<Value> = elements(args[0].clone(), name)?.collect();
            let func = args[1].clone();
            for item in items.into_iter().rev() {
                if matches!(call_function(func.clone(), vec![item.clone()])?, Value::Bool(true)) {
                    return Ok(Value::tag("Ok", [item]));
                }
            }
            Ok(Value::tag("Err", [Value::bare("NotFound")]))
        }
        // `map` whose transform also gets the element's position, as a `U64`.
        "map_with_index" => {
            expect(2, args.len())?;
            let items = elements(args[0].clone(), name)?;
            let func = args[1].clone();
            let mut out = Vec::new();
            for (i, item) in items.enumerate() {
                out.push(call_function(func.clone(), vec![item, Value::Int(i as i128)])?);
            }
            Ok(Value::list(out))
        }
        // The INDEX of the first match rather than the item, which is what a caller
        // that goes on to slice the list needs.
        "find_first_index" | "find_last_index" => {
            expect(2, args.len())?;
            let items: Vec<Value> = elements(args[0].clone(), name)?.collect();
            let func = args[1].clone();
            let mut found: Option<usize> = None;
            for (i, item) in items.iter().enumerate() {
                if matches!(call_function(func.clone(), vec![item.clone()])?, Value::Bool(true)) {
                    found = Some(i);
                    if name == "find_first_index" {
                        break;
                    }
                }
            }
            Ok(match found {
                Some(i) => Value::tag("Ok", [Value::Int(i as i128)]),
                None => Value::tag("Err", [Value::bare("NotFound")]),
            })
        }
        "sum" | "product" => {
            expect(1, args.len())?;
            let mut items = elements(args[0].clone(), name)?;
            let Some(mut acc) = items.next() else {
                return Ok(if name == "sum" {
                    Value::Int(0)
                } else {
                    Value::tag("Err", [Value::bare("IterWasEmpty")])
                });
            };
            let op = if name == "sum" { BinOp::Add } else { BinOp::Mul };
            for item in items {
                acc = apply_binop(op, &acc, &item)?;
            }
            Ok(if name == "sum" { acc } else { Value::tag("Ok", [acc]) })
        }
        "min" | "max" => {
            expect(1, args.len())?;
            let mut items = elements(args[0].clone(), name)?;
            let Some(mut best) = items.next() else {
                return Ok(Value::tag("Err", [Value::bare("IterWasEmpty")]));
            };
            for item in items {
                let ordering = order_values(&item, &best);
                if (name == "min" && ordering == Some(std::cmp::Ordering::Less))
                    || (name == "max" && ordering == Some(std::cmp::Ordering::Greater))
                {
                    best = item;
                }
            }
            Ok(Value::tag("Ok", [best]))
        }
        "sort" | "sort_reversed" | "sort_by" | "sort_by_reversed" | "sort_with" | "sort_with_reversed" => {
            let has_fn = name != "sort" && name != "sort_reversed";
            expect(if has_fn { 2 } else { 1 }, args.len())?;
            let mut items = as_list(&mut args[0])?;
            let func = args.get(1).cloned();
            let mut failed: Option<EvalError> = None;
            let mut key = |v: &Value| -> Value {
                match (&func, name) {
                    (Some(f), "sort_by" | "sort_by_reversed") => match call_function(f.clone(), vec![v.clone()]) {
                        Ok(k) => k,
                        Err(e) => {
                            failed = Some(e);
                            Value::Unit
                        }
                    },
                    _ => v.clone(),
                }
            };
            let keyed: Vec<(Value, Value)> = items.drain(..).map(|v| (key(&v), v)).collect();
            if let Some(e) = failed {
                return Err(e);
            }
            let mut keyed = keyed;
            let mut failed: Option<EvalError> = None;
            keyed.sort_by(|(ka, a), (kb, b)| {
                if failed.is_some() {
                    return std::cmp::Ordering::Equal;
                }
                let ordering = match (&func, name) {
                    (Some(f), "sort_with" | "sort_with_reversed") => {
                        match call_function(f.clone(), vec![a.clone(), b.clone()]) {
                            Ok(Value::Tag("Before", _)) => std::cmp::Ordering::Less,
                            Ok(Value::Tag("After", _)) => std::cmp::Ordering::Greater,
                            Ok(_) => std::cmp::Ordering::Equal,
                            Err(e) => {
                                failed = Some(e);
                                std::cmp::Ordering::Equal
                            }
                        }
                    }
                    _ => order_values(ka, kb).unwrap_or(std::cmp::Ordering::Equal),
                };
                if name.ends_with("_reversed") { ordering.reverse() } else { ordering }
            });
            if let Some(e) = failed {
                return Err(e);
            }
            Ok(Value::list(keyed.into_iter().map(|(_, v)| v).collect()))
        }
        "sublist" => {
            expect(2, args.len())?;
            let items = peek(&args[0], name)?;
            let (start, len) = record_start_len(&args[1]).ok_or_else(|| EvalError {
                message: format!("List.sublist needs {{ start, len }}, got {}", args[1]),
            })?;
            let start = start.min(items.len());
            let end = start.saturating_add(len).min(items.len());
            Ok(Value::list(items[start..end].to_vec()))
        }
        "append_sublist" => {
            expect(3, args.len())?;
            let (start, len) = record_start_len(&args[2]).ok_or_else(|| EvalError {
                message: format!("List.append_sublist needs {{ start, len }}, got {}", args[2]),
            })?;
            let source = peek(&args[1], name)?;
            let start = start.min(source.len());
            let end = start.saturating_add(len).min(source.len());
            let slice = source[start..end].to_vec();
            let mut items = as_list(&mut args[0])?;
            items.extend(slice);
            Ok(Value::list(items))
        }
        "drop_at" | "drop_swap" => {
            expect(2, args.len())?;
            let mut items = as_list(&mut args[0])?;
            if let Some(i) = as_index(&args[1]) {
                if i < items.len() {
                    if name == "drop_at" {
                        items.remove(i);
                    } else {
                        items.swap_remove(i);
                    }
                }
            }
            Ok(Value::list(items))
        }
        "clear" => {
            expect(1, args.len())?;
            Ok(Value::list(Vec::new()))
        }
        "split_at" => {
            expect(2, args.len())?;
            let mut items = as_list(&mut args[0])?;
            let at = as_index(&args[1]).unwrap_or(0).min(items.len());
            let others = items.split_off(at);
            Ok(Value::record(vec![("before", Value::list(items)), ("others", Value::list(others))]))
        }
        "join" => {
            expect(1, args.len())?;
            let mut out = Vec::new();
            for inner in elements(args[0].clone(), name)? {
                out.extend(elements(inner, name)?);
            }
            Ok(Value::list(out))
        }
        // `List.join(List.map(list, transform))`, as `Builtin.roc` defines it.
        "join_map" => {
            expect(2, args.len())?;
            let items = elements(args[0].clone(), name)?;
            let func = args[1].clone();
            let mut out = Vec::new();
            for item in items {
                out.extend(elements(call_function(func.clone(), vec![item])?, name)?);
            }
            Ok(Value::list(out))
        }
        "keep_oks" => {
            expect(2, args.len())?;
            let items = elements(args[0].clone(), name)?;
            let func = args[1].clone();
            let mut out = Vec::new();
            for item in items {
                if let Value::Tag("Ok", payload) = call_function(func.clone(), vec![item])? {
                    out.extend(payload.first().cloned());
                }
            }
            Ok(Value::list(out))
        }
        // The indexed writes: `Ok(list)` in bounds, `Err(OutOfBounds)` otherwise.
        "set" | "swap" | "update" | "insert" | "replace" => {
            expect(3, args.len())?;
            let mut items = as_list(&mut args[0])?;
            let index = as_index(&args[1]).unwrap_or(usize::MAX);
            let in_bounds = if name == "insert" { index <= items.len() } else { index < items.len() };
            let out_of_bounds = || Ok(Value::tag("Err", [Value::bare("OutOfBounds")]));
            if !in_bounds {
                return out_of_bounds();
            }
            match name {
                "set" => items[index] = args[2].clone(),
                "insert" => items.insert(index, args[2].clone()),
                "swap" => match as_index(&args[2]) {
                    Some(j) if j < items.len() => items.swap(index, j),
                    _ => return out_of_bounds(),
                },
                "update" => {
                    let updated = call_function(args[2].clone(), vec![items[index].clone()])?;
                    items[index] = updated;
                }
                _ => {
                    let prev = std::mem::replace(&mut items[index], args[2].clone());
                    return Ok(Value::tag(
                        "Ok",
                        vec![Value::record(vec![("list", Value::list(items)), ("prev", prev)])],
                    ));
                }
            }
            Ok(Value::tag("Ok", [Value::list(items)]))
        }
        // A range knows its length from its bounds, so neither of these walks anything.
        "len" => {
            expect(1, args.len())?;
            let count = Elements::count_of(&args[0]).ok_or_else(|| EvalError {
                message: format!("List.{} needs a List, got {}", name, args[0]),
            })?;
            Ok(Value::Int(count as i128))
        }
        "is_empty" => {
            expect(1, args.len())?;
            let count = Elements::count_of(&args[0]).ok_or_else(|| EvalError {
                message: format!("List.{} needs a List, got {}", name, args[0]),
            })?;
            Ok(Value::Bool(count == 0))
        }
        "map" => {
            expect(2, args.len())?;
            let capacity = Elements::count_of(&args[0]).unwrap_or(0);
            let items = elements(args[0].clone(), name)?;
            let func = args[1].clone();
            // The OUTPUT is a list either way, but the input need not become one first.
            let mut out = Vec::with_capacity(capacity);
            for item in items {
                out.push(call_function(func.clone(), vec![item])?);
            }
            Ok(Value::list(out))
        }
        "fold" => {
            expect(3, args.len())?;
            let items = elements(args[0].clone(), name)?;
            let mut acc = args[1].clone();
            let func = args[2].clone();
            for item in items {
                acc = call_function(func.clone(), vec![acc, item])?;
            }
            Ok(acc)
        }
        "concat" => {
            expect(2, args.len())?;
            let mut items = as_list(&mut args[0])?;
            items.extend(as_list(&mut args[1])?);
            Ok(Value::list(items))
        }
        "append" => {
            expect(2, args.len())?;
            let mut items = as_list(&mut args[0])?;
            items.push(args[1].clone());
            Ok(Value::list(items))
        }
        "prepend" => {
            expect(2, args.len())?;
            let mut items = vec![args[1].clone()];
            items.extend(as_list(&mut args[0])?);
            Ok(Value::list(items))
        }
        // roc spells it `rev`; `reverse` is kept because this interpreter answered to
        // it before the name was checked against `Builtin.roc`.
        "rev" | "reverse" => {
            expect(1, args.len())?;
            let mut items = as_list(&mut args[0])?;
            items.reverse();
            Ok(Value::list(items))
        }
        // `Ok(first)` or `Err(ListWasEmpty)`, matching roc.
        "first" | "last" => {
            expect(1, args.len())?;
            let items = peek(&args[0], name)?;
            let picked = if name == "first" { items.first() } else { items.last() };
            Ok(match picked {
                Some(v) => Value::tag("Ok", [v.clone()]),
                None => Value::tag("Err", [Value::bare("ListWasEmpty")]),
            })
        }
        "get" => {
            expect(2, args.len())?;
            let items = peek(&args[0], name)?;
            let index = as_index(&args[1]).unwrap_or(usize::MAX);
            Ok(match items.get(index) {
                Some(v) => Value::tag("Ok", [v.clone()]),
                None => Value::tag("Err", [Value::bare("OutOfBounds")]),
            })
        }
        "keep_if" | "drop_if" => {
            expect(2, args.len())?;
            let items = elements(args[0].clone(), name)?;
            let func = args[1].clone();
            let keep = name == "keep_if";
            let mut out = Vec::new();
            for item in items {
                if matches!(call_function(func.clone(), vec![item.clone()])?, Value::Bool(b) if b == keep)
                {
                    out.push(item);
                }
            }
            Ok(Value::list(out))
        }
        "take_first" | "take_last" | "drop_first" | "drop_last" => {
            expect(2, args.len())?;
            let items = peek(&args[0], name)?;
            let n = as_index(&args[1]).unwrap_or(0).min(items.len());
            let taken = match name {
                "take_first" => items[..n].to_vec(),
                "take_last" => items[items.len() - n..].to_vec(),
                "drop_first" => items[n..].to_vec(),
                _ => items[..items.len() - n].to_vec(),
            };
            Ok(Value::list(taken))
        }
        // `fold_try` stops at the first `Err`, which is the whole point: the
        // accumulator is a Try and the fold short-circuits.
        "fold_try" => {
            expect(3, args.len())?;
            let items = elements(args[0].clone(), name)?;
            let mut acc = args[1].clone();
            let func = args[2].clone();
            for item in items {
                match call_function(func.clone(), vec![acc.clone(), item])? {
                    Value::Tag("Ok", payload) => {
                        acc = payload.first().cloned().unwrap_or(Value::Unit)
                    }
                    stop @ Value::Tag("Err", _) => return Ok(stop),
                    other => acc = other,
                }
            }
            Ok(Value::tag("Ok", [acc]))
        }
        // Collecting an iterator into a list. A range reaches here still a range —
        // `.iter()` no longer materializes one — so this is where it becomes a list.
        "from_iter" => {
            expect(1, args.len())?;
            if let Value::Iter(l) = &args[0] {
                return Ok(Value::list(lazy::materialize(l)?));
            }
            Ok(Value::list(elements(args[0].clone(), name)?.collect()))
        }
        "contains" => {
            expect(2, args.len())?;
            let mut items = elements(args[0].clone(), name)?;
            Ok(Value::Bool(items.any(|v| values_equal(&v, &args[1]))))
        }
        // Does the list begin (end) with every element of the second, in order?
        "starts_with" | "ends_with" => {
            expect(2, args.len())?;
            let items: Vec<Value> = elements(args[0].clone(), name)?.collect();
            let part: Vec<Value> = elements(args[1].clone(), name)?.collect();
            if part.len() > items.len() {
                return Ok(Value::Bool(false));
            }
            let from = if name == "starts_with" { 0 } else { items.len() - part.len() };
            Ok(Value::Bool(part.iter().zip(&items[from..]).all(|(p, v)| values_equal(v, p))))
        }
        _ => Err(EvalError {
            message: format!("Unknown function List.{}", name),
        }),
    }
}

/// An iterator's `len_if_known`: `Known(n)` for anything whose length is already
/// settled, `Unknown` for a lazy one that has to be walked to find out.
///
/// roc's `Iter` is a record with this field; rocflight's is the list, range or lazy
/// value being walked, so the VM's `GetField` answers it from here.
pub fn size_hint_of(value: &Value) -> Result<Value, EvalError> {
    if matches!(value, Value::Iter(_)) {
        let mut args = vec![value.clone()];
        if let Some(hint) = lazy::call("size_hint", &mut args) {
            return hint;
        }
    }
    match Elements::count_of(value) {
        Some(count) => Ok(Value::tag("Known", [Value::Int(count as i128)])),
        None => Ok(Value::bare("Unknown")),
    }
}

/// `{ start, len }`, as `List.sublist` and `List.append_sublist` take it.
fn record_start_len(value: &Value) -> Option<(usize, usize)> {
    let Value::Record(fields) = value else { return None };
    let field = |name: &str| match fields.iter().find(|(n, _)| *n == name)?.1 {
        Value::Int(n) if n >= 0 => Some(n as usize),
        _ => None,
    };
    Some((field("start")?, field("len")?))
}

/// How two values order, for `sort`, `min` and `max`: numbers by value, strings by
/// bytes, lists and tuples element by element, tags by name and then payload.
fn order_values(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Some(x.cmp(y)),
        (Value::U128(x), Value::U128(y)) => Some(x.cmp(y)),
        (Value::U128(x), Value::Int(y)) => Some(x.cmp(&(*y as u128))),
        (Value::Int(x), Value::U128(y)) => Some((*x as u128).cmp(y)),
        (Value::Dec(x), Value::Dec(y)) => Some(x.cmp(y)),
        (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
        (Value::List(x), Value::List(y)) => order_seq(x, y),
        (Value::Tuple(x), Value::Tuple(y)) => order_seq(x, y),
        (Value::Tag(x, px), Value::Tag(y, py)) => match x.cmp(y) {
            Ordering::Equal => order_seq(px, py),
            other => Some(other),
        },
        _ => match (as_dec(a), as_dec(b)) {
            (Some(x), Some(y)) if !matches!(a, Value::Float(_) | Value::F32(_)) && !matches!(b, Value::Float(_) | Value::F32(_)) => Some(x.cmp(&y)),
            _ => as_f64(a)?.partial_cmp(&as_f64(b)?),
        },
    }
}

fn order_seq(xs: &[Value], ys: &[Value]) -> Option<std::cmp::Ordering> {
    for (x, y) in xs.iter().zip(ys) {
        match order_values(x, y)? {
            std::cmp::Ordering::Equal => continue,
            other => return Some(other),
        }
    }
    Some(xs.len().cmp(&ys.len()))
}

/// The `Str` methods beyond the original handful: prefixes, suffixes, bytes.
fn call_str_more(name: &str, args: &[Value]) -> Option<Result<Value, EvalError>> {
    if name == "from_interpolation" {
        return Some(numeral::from_interpolation(args).ok_or_else(|| EvalError {
            message: "Str.from_interpolation takes a Str and a list of (Str, Str)".to_string(),
        }));
    }
    let text = match args.first() {
        Some(Value::Str(t)) => Some(&**t),
        _ => None,
    };
    let second = match args.get(1) {
        Some(Value::Str(t)) => Some(&**t),
        _ => None,
    };
    let not_found = || Ok(Value::tag("Err", [Value::bare("NotFound")]));
    Some(match name {
        "with_capacity" => Ok(str_value(String::new())),
        "reserve" | "release_excess_capacity" => Ok(Value::Str(text?.into())),
        "capacity" => Ok(Value::Int(text?.len() as i128)),
        "repeat" => {
            Ok(str_value(text?.repeat(as_index(args.get(1)?)?)))
        }
        "caseless_ascii_equals" => Ok(Value::Bool(text?.eq_ignore_ascii_case(second?))),
        "drop_prefix" => Ok(str_value(text?.strip_prefix(second?).unwrap_or(text?).to_string())),
        "drop_suffix" => Ok(str_value(text?.strip_suffix(second?).unwrap_or(text?).to_string())),
        "with_prefix" => Ok(str_value(format!("{}{}", second?, text?))),
        "with_suffix" => Ok(str_value(format!("{}{}", text?, second?))),
        "drop_prefix_caseless_ascii" | "drop_suffix_caseless_ascii" => {
            let (haystack, needle) = (text?, second?);
            if haystack.len() < needle.len() {
                return Some(not_found());
            }
            let (kept, checked) = if name.starts_with("drop_prefix") {
                (&haystack[needle.len()..], &haystack[..needle.len()])
            } else {
                (&haystack[..haystack.len() - needle.len()], &haystack[haystack.len() - needle.len()..])
            };
            if checked.eq_ignore_ascii_case(needle) {
                Ok(Value::tag("Ok", [str_value(kept.to_string())]))
            } else {
                not_found()
            }
        }
        "iter_utf8" => Ok(lazy::iter_utf8(text?)),
        "from_utf8_lossy" => {
            let Value::List(items) = args.first()? else { return None };
            let bytes: Vec<u8> = items.iter().filter_map(|v| if let Value::Int(n) = v { u8::try_from(*n).ok() } else { None }).collect();
            Ok(str_value(String::from_utf8_lossy(&bytes).into_owned()))
        }
        "drop_first_bytes" | "drop_last_bytes" => {
            let n = as_index(args.get(1)?)?;
            let text = text?;
            if n >= text.len() {
                return Some(Ok(Value::tag("Ok", [str_value(String::new())])));
            }
            let kept = if name == "drop_first_bytes" { text.get(n..) } else { text.get(..text.len() - n) };
            Ok(match kept {
                Some(rest) => Value::tag("Ok", [str_value(rest.to_string())]),
                None => Value::tag("Err", [Value::bare("BadUtf8")]),
            })
        }
        "split_last" => {
            let (haystack, needle) = (text?, second?);
            match haystack.rfind(needle) {
                Some(at) => Ok(Value::tag(
                    "Ok",
                    vec![Value::record(vec![
                        ("before", str_value(haystack[..at].to_string())),
                        ("after", str_value(haystack[at + needle.len()..].to_string())),
                    ])],
                )),
                None => not_found(),
            }
        }
        _ => return None,
    })
}

/// Call an effect provided by the default (platformless) host.
///
/// `echo!` writes its argument to stdout with no trailing newline — matching
/// `roc run`, which is why the test .roc files spell newlines explicitly.
/// One whole unit of a `Dec`: 10^18, roc's own scale.
///
/// `roc-compiler/src/builtins/dec.zig` sets `decimal_places: u5 = 18`, so every `Dec`
/// is an `i128` holding the value times this.
pub const DEC_SCALE: i128 = 1_000_000_000_000_000_000;

/// Parse a `Dec` exactly: `"200000.0"`, `"-3.5"`, `"12"`, `"2e5"`, `"1.5e-3"`. A
/// digit past the eighteenth fractional place, or anything that is not digits around
/// one point and one exponent, is `None`.
pub fn dec_from_str(text: &str) -> Option<i128> {
    let (negative, body) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let (mantissa, exponent) = match body.split_once(|c| c == 'e' || c == 'E') {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i32>().ok()?),
        None => (body, 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    if !whole.bytes().all(|b| b.is_ascii_digit()) || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // Every digit, then shift the point to eighteen places: pad with zeros, or drop
    // trailing digits that are all zero.
    let mut digits = format!("{}{}", whole, fraction);
    let places = fraction.len() as i64 - i64::from(exponent);
    if places < 18 {
        digits.push_str(&"0".repeat((18 - places) as usize));
    } else if places > 18 {
        let cut = (places - 18) as usize;
        if cut > digits.len() || digits[digits.len() - cut..].bytes().any(|b| b != b'0') {
            return None;
        }
        digits.truncate(digits.len() - cut);
    }
    let magnitude: u128 = if digits.is_empty() { 0 } else { digits.parse().ok()? };
    if negative {
        // `Dec.lowest` is one past `-i128::MAX`.
        (magnitude <= i128::MAX as u128 + 1).then(|| (magnitude as i128).wrapping_neg())
    } else {
        i128::try_from(magnitude).ok()
    }
}

/// Render a `Dec` the way roc does: the whole part, a point, and the fraction with its
/// trailing zeros removed. A whole value keeps a single `.0`.
pub fn dec_to_string(raw: i128) -> String {
    let negative = raw < 0;
    let magnitude = raw.unsigned_abs();
    let whole = magnitude / DEC_SCALE as u128;
    let fraction = magnitude % DEC_SCALE as u128;
    let mut digits = format!("{:018}", fraction);
    while digits.ends_with('0') && digits.len() > 1 {
        digits.pop();
    }
    format!("{}{}.{}", if negative { "-" } else { "" }, whole, digits)
}

/// A `Dec` from the double a float literal parsed to.
///
/// Rounded at the eighteenth place, which is where a `Dec` stops.
pub fn dec_from_f64(value: f64) -> i128 {
    (value * DEC_SCALE as f64).round() as i128
}

/// Read a `Dec` out of a value, converting a whole number if that is what it is.
///
/// An integer literal in a `List(Dec)` reaches the evaluator as a `Dec` already — the
/// checker said so — but a length or a count arrives as an ordinary integer.
pub fn as_dec(value: &Value) -> Option<i128> {
    match value {
        Value::Dec(raw) => Some(*raw),
        Value::Int(n) => n.checked_mul(DEC_SCALE),
        // A float beside a `Dec` widens rather than dragging the `Dec` down to an f64:
        // the checker cannot always say that a fold's accumulator is fixed point, and
        // the alternative is losing the digits the type exists to keep.
        Value::Float(f) => Some(dec_from_f64(*f)),
        Value::F32(f) => Some(dec_from_f64(f64::from(*f))),
        _ => None,
    }
}

/// A number as an f64: an integer, either float width, or a `Dec` — lossily, because a
/// numeral that reached a float function through an untyped parameter is a `Dec` here
/// where roc would have inferred the float.
fn as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Int(n) => Some(*n as f64),
        Value::Float(f) => Some(*f),
        Value::F32(f) => Some(f64::from(*f)),
        Value::Dec(raw) => dec_to_string(*raw).parse().ok(),
        _ => None,
    }
}

/// A count or an index: a whole number that is not negative.
pub(crate) fn as_index(value: &Value) -> Option<usize> {
    usize::try_from(as_whole(value)?).ok()
}

/// A whole number: an integer, or a `Dec` with nothing after the point, which is what
/// an integer numeral becomes when nothing typed it.
fn as_whole(value: &Value) -> Option<i128> {
    match value {
        Value::Int(n) => Some(*n),
        Value::U128(n) => i128::try_from(*n).ok(),
        Value::Dec(raw) if raw % DEC_SCALE == 0 => Some(raw / DEC_SCALE),
        _ => None,
    }
}

/// Any integer value as its `u128` bit pattern: a `U128` is itself, and an `Int`
/// reinterprets its bits (a small `U128` may still arrive as one).
pub fn as_u128_bits(value: &Value) -> Option<u128> {
    match value {
        Value::U128(n) => Some(*n),
        Value::Int(n) => Some(*n as u128),
        Value::Dec(raw) if raw % DEC_SCALE == 0 && *raw >= 0 => Some((raw / DEC_SCALE) as u128),
        _ => None,
    }
}

/// `U128`, as a genuine `u128` rather than an `i128` bit pattern: the operations whose
/// answer differs above `i128::MAX` — comparison, `to_str`, the logical shifts,
/// `from_str` — plus the ones that just need the value back as `Value::U128`.
fn call_u128(method: &str, args: &[Value]) -> Option<Result<Value, EvalError>> {
    let u = |v: &Value| as_u128_bits(v);
    let out = |n: u128| Ok(Value::U128(n));
    match method {
        "highest" => return Some(out(u128::MAX)),
        "lowest" => return Some(out(0)),
        "from_str" => {
            let Value::Str(text) = args.first()? else { return None };
            let parsed = text.trim().parse::<u128>().ok();
            return Some(Ok(match parsed {
                Some(n) => Value::tag("Ok", [Value::U128(n)]),
                None => Value::tag("Err", [Value::bare("BadNumStr")]),
            }));
        }
        _ => {}
    }
    let a = u(args.first()?)?;
    let b = args.get(1).and_then(u);
    let crash = |what: &str| Err(EvalError { message: format!("crash: U128.{} {}", method, what) });
    Some(match (method, b) {
        ("range_exclusive_to", Some(b)) => Ok(Value::Iter(std::rc::Rc::new(lazy::Lazy::Range {
            at: Value::U128(a), end: Value::U128(b), step: Value::U128(1), inclusive: false,
        }))),
        ("range_inclusive_to", Some(b)) => Ok(Value::Iter(std::rc::Rc::new(lazy::Lazy::Range {
            at: Value::U128(a), end: Value::U128(b), step: Value::U128(1), inclusive: true,
        }))),
        ("to_str", _) => Ok(str_value(a.to_string())),
        ("to_f64", _) => Ok(Value::Float(a as f64)),
        ("to_f32", _) | ("to_f32_wrap", _) => Ok(Value::F32(a as f32)),
        ("to_dec", _) => Ok(Value::Dec((a as i128).saturating_mul(DEC_SCALE))),
        ("to_inspect", _) => Ok(str_value(a.to_string())),
        ("is_eq", Some(b)) => Ok(Value::Bool(a == b)),
        ("is_ne", Some(b)) => Ok(Value::Bool(a != b)),
        ("is_lt", Some(b)) => Ok(Value::Bool(a < b)),
        ("is_lte", Some(b)) => Ok(Value::Bool(a <= b)),
        ("is_gt", Some(b)) => Ok(Value::Bool(a > b)),
        ("is_gte", Some(b)) => Ok(Value::Bool(a >= b)),
        ("order_relative_to", Some(b)) => Ok(Value::tag(
            match a.cmp(&b) {
                std::cmp::Ordering::Less => "Before",
                std::cmp::Ordering::Greater => "After",
                std::cmp::Ordering::Equal => "Same",
            }, vec![],
        )),
        ("min", Some(b)) => out(a.min(b)),
        ("max", Some(b)) => out(a.max(b)),
        ("abs_diff", Some(b)) => out(a.abs_diff(b)),
        ("is_zero", _) => Ok(Value::Bool(a == 0)),
        ("is_even", _) => Ok(Value::Bool(a % 2 == 0)),
        ("is_odd", _) => Ok(Value::Bool(a % 2 != 0)),
        ("bitwise_and", Some(b)) => out(a & b),
        ("bitwise_or", Some(b)) => out(a | b),
        ("bitwise_xor", Some(b)) => out(a ^ b),
        ("bitwise_not", _) => out(!a),
        ("shl_wrap", Some(b)) => out(a << (b % 128)),
        ("shr_wrap", Some(b)) => out(a >> (b % 128)),
        ("shr_zf_wrap", Some(b)) => out(a >> (b % 128)),
        ("count_one_bits", _) => Ok(Value::Int(i128::from(a.count_ones()))),
        ("count_leading_zero_bits", _) => Ok(Value::Int(i128::from(a.leading_zeros()))),
        ("count_trailing_zero_bits", _) => Ok(Value::Int(i128::from(a.trailing_zeros()))),
        ("plus", Some(b)) => match a.checked_add(b) { Some(n) => out(n), None => crash("overflowed") },
        ("minus", Some(b)) => match a.checked_sub(b) { Some(n) => out(n), None => crash("overflowed") },
        ("times", Some(b)) => match a.checked_mul(b) { Some(n) => out(n), None => crash("overflowed") },
        ("plus_wrap", Some(b)) => out(a.wrapping_add(b)),
        ("minus_wrap", Some(b)) => out(a.wrapping_sub(b)),
        ("times_wrap", Some(b)) => out(a.wrapping_mul(b)),
        ("plus_try", Some(b)) | ("minus_try", Some(b)) | ("times_try", Some(b)) => {
            let r = match method {
                "plus_try" => a.checked_add(b),
                "minus_try" => a.checked_sub(b),
                _ => a.checked_mul(b),
            };
            Ok(match r {
                Some(n) => Value::tag("Ok", [Value::U128(n)]),
                None => Value::tag("Err", [Value::bare("Overflow")]),
            })
        }
        ("plus_saturated", Some(b)) => out(a.checked_add(b).unwrap_or(u128::MAX)),
        ("minus_saturated", Some(b)) => out(a.checked_sub(b).unwrap_or(0)),
        ("times_saturated", Some(b)) => out(a.checked_mul(b).unwrap_or(u128::MAX)),
        ("pow", Some(b)) => match u32::try_from(b).ok().and_then(|e| a.checked_pow(e)) {
            Some(n) => out(n),
            None => crash("overflowed"),
        },
        ("pow_try", Some(b)) => Ok(match u32::try_from(b).ok().and_then(|e| a.checked_pow(e)) {
            Some(n) => Value::tag("Ok", [Value::U128(n)]),
            None => Value::tag("Err", [Value::bare("Overflow")]),
        }),
        ("div_try", Some(0)) | ("div_ceil_try", Some(0)) | ("div_floor_try", Some(0)) => {
            Ok(Value::tag("Err", [Value::bare("DivByZero")]))
        }
        ("div_try", Some(b)) => Ok(Value::tag("Ok", [Value::U128(a / b)])),
        // Unsigned, so ceil/floor of a division differ from trunc only when there is a
        // remainder; both round toward +inf here because operands are non-negative.
        ("div_ceil_try", Some(b)) => Ok(Value::tag("Ok", [Value::U128(a / b + u128::from(a % b != 0))])),
        ("div_floor_try", Some(b)) => Ok(Value::tag("Ok", [Value::U128(a / b)])),
        ("div_ceil_by", Some(b)) if b != 0 => out(a / b + u128::from(a % b != 0)),
        ("div_floor_by", Some(b)) if b != 0 => out(a / b),
        ("mod_by", Some(b)) if b != 0 => out(a % b),
        ("plus_overflows", Some(b)) => Ok(Value::Bool(a.checked_add(b).is_none())),
        ("minus_overflows", Some(b)) => Ok(Value::Bool(a.checked_sub(b).is_none())),
        ("times_overflows", Some(b)) => Ok(Value::Bool(a.checked_mul(b).is_none())),
        ("to_hash", _) => Ok(Value::U128(a)),
        ("div_by", Some(0)) | ("div_trunc_by", Some(0)) | ("rem_by", Some(0)) => crash("divided by zero"),
        ("div_by", Some(b)) | ("div_trunc_by", Some(b)) => out(a / b),
        ("rem_by", Some(b)) => out(a % b),
        _ => return None,
    })
}

/// `Dec` multiplication, without a 256-bit intermediate.
///
/// The naive `a * b / SCALE` overflows an i128 for anything past about 13: the operands
/// already carry 10^18 each, so their product carries 10^36 and `25.0 * 25.0` is
/// 6.25e38 against an i128 ceiling of 1.7e38. Splitting each operand into whole and
/// fractional parts keeps every term in range:
///
/// ```text
/// (qa·S + ra)(qb·S + rb) / S  =  qa·qb·S + qa·rb + ra·qb + ra·rb/S
/// ```
///
/// The last term is where the digits past the eighteenth go, which is exactly where a
/// `Dec` drops them anyway.
pub fn dec_mul(a: i128, b: i128) -> Option<i128> {
    let (qa, ra) = (a / DEC_SCALE, a % DEC_SCALE);
    let (qb, rb) = (b / DEC_SCALE, b % DEC_SCALE);
    qa.checked_mul(qb)?
        .checked_mul(DEC_SCALE)?
        .checked_add(qa.checked_mul(rb)?)?
        .checked_add(ra.checked_mul(qb)?)?
        .checked_add(ra.checked_mul(rb)? / DEC_SCALE)
}

/// `Dec` division, split the same way: `a/b·S` as a whole part and a remainder.
pub fn dec_div(a: i128, b: i128) -> Option<i128> {
    if b == 0 {
        return None;
    }
    let (whole, remainder) = (a / b, a % b);
    whole
        .checked_mul(DEC_SCALE)?
        .checked_add(remainder.checked_mul(DEC_SCALE)? / b)
}

/// `Dec` arithmetic. Fixed point, so a product and a quotient rescale.
///
/// Saturating rather than wrapping: roc's `times_saturated` is written against that,
/// and `SafeMath` detects an overflow by comparing against `Dec.highest`.
pub fn dec_binop(op: BinOp, a: i128, b: i128) -> Option<Result<Value, EvalError>> {
    // Past `Dec.highest` roc crashes, on every backend; the saturating forms are the
    // `plus_saturated` family, not the operators.
    let checked = |v: Option<i128>| match v {
        Some(raw) => Ok(Value::Dec(raw)),
        None => Err(EvalError { message: "crash: Dec overflowed".to_string() }),
    };
    Some(Ok(match op {
        BinOp::Add => return Some(checked(a.checked_add(b))),
        BinOp::Sub => return Some(checked(a.checked_sub(b))),
        BinOp::Mul => return Some(checked(dec_mul(a, b))),
        BinOp::Div => {
            if b == 0 {
                return Some(Err(EvalError { message: "crash: Dec division by zero".to_string() }));
            }
            return Some(checked(dec_div(a, b)));
        }
        // `//` truncates to a whole `Dec`; `%` is the remainder with the dividend's sign.
        BinOp::IntDiv => {
            if b == 0 {
                return Some(Err(EvalError { message: "crash: Dec division by zero".to_string() }));
            }
            return Some(checked((a / b).checked_mul(DEC_SCALE)));
        }
        BinOp::Rem => {
            if b == 0 {
                return Some(Err(EvalError { message: "crash: Dec division by zero".to_string() }));
            }
            Value::Dec(a % b)
        }
        BinOp::Lt => Value::Bool(a < b),
        BinOp::Gt => Value::Bool(a > b),
        BinOp::Le => Value::Bool(a <= b),
        BinOp::Ge => Value::Bool(a >= b),
        BinOp::Eq => Value::Bool(a == b),
        BinOp::Ne => Value::Bool(a != b),
        _ => return None,
    }))
}

/// Mix bytes into a hasher's state. FNV-1a, 64-bit.
///
/// `Builtin.roc` declares every `Hasher.write_*` as an intrinsic and says only that a
/// hash must be consistent — the algorithm is the compiler's business, and `Dict`'s
/// fingerprints and bucket indices are computed from whatever it returns. FNV-1a is
/// small, has no state beyond the accumulator, and is what a `{ state : U64 }` hasher
/// can hold.
fn hash_mix(state: u64, bytes: &[u8]) -> u64 {
    let mut hash = state;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

/// Read a `Hasher`'s state, mix `bytes` in, and hand back the new one.
fn hasher_write(hasher: &Value, bytes: &[u8]) -> Result<Value, EvalError> {
    let state = match hasher {
        Value::Record(fields) => fields
            .iter()
            .find(|(name, _)| *name == "state")
            .and_then(|(_, value)| match value {
                Value::Int(n) => Some(*n as u64),
                _ => None,
            }),
        _ => None,
    };
    let state = state.ok_or_else(|| EvalError {
        message: format!("a Hasher is a record with a `state`, got {}", hasher),
    })?;
    Ok(Value::record(vec![("state", Value::Int(hash_mix(state, bytes) as i128))]))
}

/// The bytes a value contributes to a hash.
///
/// Two values that are `==` must contribute the same bytes, which is the whole contract
/// a `Dict` relies on.
fn hash_bytes(value: &Value) -> Option<Vec<u8>> {
    // A component that is a nominal with its own `to_hash` — a `Dict`, a `Set` — hashes
    // by that method, whose result is INDEPENDENT of insertion order and ignores the
    // bucket layout that two equal dicts differ in. Its erased layout (a `HashMap` tag
    // over a record of `entries`/`buckets`/`shifts`) would hash order-dependently, so a
    // dict nested in a record/tuple/tag key would miss on lookup. The method hashes
    // entries, never the whole container, so this terminates.
    if matches!(value, Value::Tag(..) | Value::Record(_)) {
        if let Some((_, func)) = crate::vm::best_method("to_hash", value) {
            let fresh = Value::record(vec![("state", Value::Int(0xcbf2_9ce4_8422_2325_u64 as i128))]);
            if let Ok(Value::Record(fields)) = call_function(func, vec![value.clone(), fresh]) {
                if let Some((_, Value::Int(state))) = fields.iter().find(|(n, _)| *n == "state") {
                    return Some((*state as u64).to_le_bytes().to_vec());
                }
            }
        }
    }
    Some(match value {
        // A whole `Dec` hashes as the integer it equals, so a `Dec` `2.0` and an `Int`
        // `2` — which `values_equal` already calls equal — land in the same bucket:
        // that is what lets `s.insert(2)` dedup against a `U64` set member even when
        // the `2` defaulted to `Dec`.
        Value::Int(n) => n.to_le_bytes().to_vec(),
        Value::U128(n) if *n <= i128::MAX as u128 => (*n as i128).to_le_bytes().to_vec(),
        Value::U128(n) => n.to_le_bytes().to_vec(),
        Value::Dec(d) if d % DEC_SCALE == 0 => (d / DEC_SCALE).to_le_bytes().to_vec(),
        Value::Dec(d) => d.to_le_bytes().to_vec(),
        Value::Str(text) => text.as_bytes().to_vec(),
        Value::Bool(b) => vec![u8::from(*b)],
        // Every NaN hashes alike, as it inspects and compares alike.
        Value::Float(f) => (if f.is_nan() { f64::NAN } else { *f }).to_le_bytes().to_vec(),
        Value::F32(f) => (if f.is_nan() { f32::NAN } else { *f }).to_le_bytes().to_vec(),
        Value::Unit => Vec::new(),
        // A list or a tuple hashes as its elements in order; a tag as its name then
        // its payload; a record as its fields in NAME order, so `{ a: 1, b: 2 }` and
        // `{ b: 2, a: 1 }` hash alike — which is what makes a record a `Dict` key.
        Value::List(_) | Value::Tuple(_) => {
            let mut bytes = Vec::new();
            for item in value.sequence().expect("matched a sequence") {
                bytes.extend(hash_bytes(item)?);
            }
            bytes
        }
        Value::Missing => Vec::new(),
        Value::Record(fields) => {
            let mut sorted: Vec<&(&str, Value)> = fields.iter().collect();
            sorted.sort_by(|a, b| a.0.cmp(b.0));
            let mut bytes = Vec::new();
            for (name, v) in sorted {
                bytes.extend(name.as_bytes());
                bytes.extend(hash_bytes(v)?);
            }
            bytes
        }
        Value::Tag(name, payload) => {
            let mut bytes = name.as_bytes().to_vec();
            for item in payload.iter() {
                bytes.extend(hash_bytes(item)?);
            }
            bytes
        }
        _ => return None,
    })
}

/// `Hasher.write_u64(h, n)` and the fifteen siblings, plus `x.to_hash(h)`.
/// SHA-256 / BLAKE3, as roc's `Crypto` module exposes them. The parser collapses
/// `Crypto.SHA256.hash`, `Crypto.SHA256.Hasher.empty`, `Crypto.SHA256.Digest.to_hex`
/// (and BLAKE3) into these synthetic modules. A `Digest` is `Tag("CryptoDigest",
/// [List(U8)])`; a `Hasher` carries its algorithm and the bytes written so far, so
/// `finish` is pure and can be called twice.
/// The eight SIMD types by their `kind` byte (element width | `0x80` if signed).
pub fn simd_kind(module: &str) -> Option<u8> {
    Some(match module {
        "U8x16" => 8, "I8x16" => 8 | 0x80,
        "U16x8" => 16, "I16x8" => 16 | 0x80,
        "U32x4" => 32, "I32x4" => 32 | 0x80,
        "U64x2" => 64, "I64x2" => 64 | 0x80,
        _ => return None,
    })
}

pub fn simd_type_name(kind: u8) -> &'static str {
    match (kind & 0x7f, kind & 0x80 != 0) {
        (8, false) => "U8x16", (8, true) => "I8x16",
        (16, false) => "U16x8", (16, true) => "I16x8",
        (32, false) => "U32x4", (32, true) => "I32x4",
        (64, false) => "U64x2", (64, true) => "I64x2",
        _ => "Simd",
    }
}

/// The lanes of a vector, each sign-extended when the type is signed.
fn simd_lanes(kind: u8, bits: u128) -> Vec<i128> {
    let width = u32::from(kind & 0x7f);
    let signed = kind & 0x80 != 0;
    let lanes = 128 / width;
    let mask: u128 = if width == 128 { u128::MAX } else { (1u128 << width) - 1 };
    (0..lanes).map(|i| {
        let raw = (bits >> (i * width)) & mask;
        if signed && width < 128 && raw >> (width - 1) & 1 == 1 {
            (raw as i128) - (1i128 << width)
        } else {
            raw as i128
        }
    }).collect()
}

/// Pack lanes (low bits of each `i128`) back into the 128-bit value.
fn simd_pack(kind: u8, lanes: &[i128]) -> u128 {
    let width = u32::from(kind & 0x7f);
    let mask: u128 = if width == 128 { u128::MAX } else { (1u128 << width) - 1 };
    let mut bits = 0u128;
    for (i, v) in lanes.iter().enumerate() {
        bits |= ((*v as u128) & mask) << (i as u32 * width);
    }
    bits
}

pub fn simd_inspect(kind: u8, bits: u128) -> String {
    let lanes: Vec<String> = simd_lanes(kind, bits).iter().map(|v| v.to_string()).collect();
    format!("{}({})", simd_type_name(kind), lanes.join(", "))
}

/// The SIMD vector types. `U8x16.default()`, `.with_lane(v, i, x)`, `.get_lane(v, i)`,
/// `.splat(x)`, `.from_u128_bits(n)`, `.to_u128_bits(v)`, structural equality, and
/// `concat_shift_bytes`. A lane index out of range crashes, as roc's does.
/// `Range` over a third-party numeric type — the record-backed `Range(Distance)`, whose
/// element defines `range_iter`. `None` falls through to the `List`/integer-range path,
/// so a plain `Value::Range` (or any non-record receiver) is untouched.
fn call_range(method: &str, args: &[Value]) -> Option<Result<Value, EvalError>> {
    let field = |rec: &Value, want: &str| -> Option<Value> {
        match rec {
            Value::Record(fields) => {
                fields.iter().find(|(name, _)| *name == want).map(|(_, v)| v.clone())
            }
            _ => None,
        }
    };
    match method {
        // `Range.custom(config)` is a record of the six Range fields; roc rebuilds a
        // `Range.{…}` of the same fields, so the config record already IS the range.
        "custom" => {
            let config = args.first()?;
            // Only a record-shaped range is ours; anything else is the integer path.
            field(config, "lower")?;
            Some(Ok(config.clone()))
        }
        // `range.iter()` dispatches the element's own `range_iter`, exactly as
        // `Builtin.roc`'s `Range.iter` does: `lower.range_iter(upper, step, …)`.
        "iter" => {
            let range = args.first()?;
            let lower = field(range, "lower")?;
            let call_args = vec![
                lower.clone(),
                field(range, "upper")?,
                field(range, "step")?,
                field(range, "upper_bound")?,
                field(range, "direction")?,
                field(range, "len_if_known")?,
            ];
            let (_, func) = crate::vm::best_method("range_iter", &lower)?;
            Some(call_function(func, call_args))
        }
        // `Range.size_hint` is the stored length hint.
        "size_hint" => Some(Ok(field(args.first()?, "len_if_known")?)),
        _ => None,
    }
}

pub fn call_simd(module: &str, method: &str, args: &[Value]) -> Option<Result<Value, EvalError>> {
    let kind = simd_kind(module)?;
    let width = u32::from(kind & 0x7f);
    let lanes = (128 / width) as i128;
    let make = |bits: u128| Value::Simd { kind, bits };
    let vec_bits = |v: &Value| -> Option<u128> {
        match v { Value::Simd { bits, .. } => Some(*bits), _ => None }
    };
    let crash = |what: &str| Err(EvalError { message: format!("crash: {}.{} {}", module, method, what) });
    Some(match method {
        "default" => Ok(make(0)),
        "splat" => {
            let x = as_whole(args.first()?)?;
            Ok(make(simd_pack(kind, &vec![x; lanes as usize])))
        }
        "from_u128_bits" => Ok(make(as_u128_bits(args.first()?)?)),
        "to_u128_bits" => Ok(Value::U128(vec_bits(args.first()?)?)),
        "from_list" => {
            let items: Vec<i128> = args.first()?.sequence()?.iter().filter_map(as_whole).collect();
            Ok(make(simd_pack(kind, &items)))
        }
        "to_list" => Ok(Value::list(simd_lanes(kind, vec_bits(args.first()?)?).into_iter().map(Value::Int).collect())),
        "to_inspect" => Ok(str_value(simd_inspect(kind, vec_bits(args.first()?)?))),
        "is_eq" => Ok(Value::Bool(vec_bits(args.first()?)? == vec_bits(args.get(1)?)?)),
        "with_lane" => {
            let bits = vec_bits(args.first()?)?;
            let i = as_whole(args.get(1)?)?;
            if i < 0 || i >= lanes { return Some(crash("lane index out of range")); }
            let x = as_whole(args.get(2)?)?;
            let mut ls = simd_lanes(kind, bits);
            ls[i as usize] = x;
            Ok(make(simd_pack(kind, &ls)))
        }
        "get_lane" => {
            let bits = vec_bits(args.first()?)?;
            let i = as_whole(args.get(1)?)?;
            if i < 0 || i >= lanes { return Some(crash("lane index out of range")); }
            Ok(Value::Int(simd_lanes(kind, bits)[i as usize]))
        }
        "broadcast_lane" => {
            let bits = vec_bits(args.first()?)?;
            let i = as_whole(args.get(1)?)?;
            if i < 0 || i >= lanes { return Some(crash("lane index out of range")); }
            let x = simd_lanes(kind, bits)[i as usize];
            Ok(make(simd_pack(kind, &vec![x; lanes as usize])))
        }
        // `concat_shift_bytes(a, b, n)`: the 32 bytes of `a ++ b`, shifted right by
        // `n`, as 16 bytes. `n` above 16 is rejected.
        "concat_shift_bytes" => {
            let a = vec_bits(args.first()?)?;
            let b = vec_bits(args.get(1)?)?;
            let n = as_whole(args.get(2)?)?;
            if !(0..=16).contains(&n) { return Some(crash("shift count above sixteen")); }
            let mut bytes = [0u8; 32];
            bytes[..16].copy_from_slice(&a.to_le_bytes());
            bytes[16..].copy_from_slice(&b.to_le_bytes());
            let mut out = [0u8; 16];
            out.copy_from_slice(&bytes[n as usize..n as usize + 16]);
            Ok(Value::Simd { kind, bits: u128::from_le_bytes(out) })
        }
        _ => return None,
    })
}

pub fn call_crypto(module: &str, method: &str, args: &[Value]) -> Option<Result<Value, EvalError>> {
    let algo = module.strip_prefix("Sha256").map(|_| "Sha256")
        .or_else(|| module.strip_prefix("Blake3").map(|_| "Blake3"))?;
    if module != algo && !module.ends_with("Hasher") && !module.ends_with("Digest") {
        return None;
    }
    let digest_bytes = |v: &Value| -> Option<Vec<u8>> {
        match v {
            Value::Tag("CryptoDigest", payload) => byte_vec(payload.first()?),
            _ => None,
        }
    };
    let make_digest = |bytes: Vec<u8>| Value::tag("CryptoDigest", [Value::list(bytes.into_iter().map(|b| Value::Int(i128::from(b))).collect())]);
    let hash = |algo: &str, data: &[u8]| -> [u8; 32] {
        if algo == "Sha256" { crypto::sha256(data) } else { crypto::blake3(data) }
    };
    Some(match (module, method) {
        // `Sha256.hash(bytes)` / `Blake3.hash(bytes)`.
        (_, "hash") if module == algo => {
            let data = byte_vec(args.first()?)?;
            Ok(make_digest(hash(algo, &data).to_vec()))
        }
        // `hash_chunks(iter)`: concatenate the chunks, then hash.
        (_, "hash_chunks") if module == algo => {
            let mut data = Vec::new();
            let items = match &args[0] {
                Value::Iter(l) => lazy::materialize(l).ok()?,
                other => other.sequence()?.to_vec(),
            };
            for chunk in items {
                data.extend(byte_vec(&chunk)?);
            }
            Ok(make_digest(hash(algo, &data).to_vec()))
        }
        (_, "empty") if module.ends_with("Hasher") => {
            Ok(Value::tag("CryptoHasher", [str_value(algo), Value::list(Vec::new())]))
        }
        (_, "write") if module.ends_with("Hasher") => {
            let Value::Tag("CryptoHasher", payload) = &args[0] else { return None };
            let mut acc = byte_vec(payload.get(1)?)?;
            acc.extend(byte_vec(args.get(1)?)?);
            Ok(Value::tag("CryptoHasher", [payload[0].clone(), Value::list(acc.into_iter().map(|b| Value::Int(i128::from(b))).collect())]))
        }
        (_, "finish") if module.ends_with("Hasher") => {
            let Value::Tag("CryptoHasher", payload) = &args[0] else { return None };
            let acc = byte_vec(payload.get(1)?)?;
            Ok(make_digest(hash(algo, &acc).to_vec()))
        }
        (_, "to_hex") => Ok(str_value(crypto::to_hex(&digest_bytes(args.first()?)?))),
        (_, "to_bytes") => {
            let bytes = digest_bytes(args.first()?)?;
            Ok(Value::list(bytes.into_iter().map(|b| Value::Int(i128::from(b))).collect()))
        }
        (_, "is_eq") => Ok(Value::Bool(digest_bytes(args.first()?)? == digest_bytes(args.get(1)?)?)),
        (_, "from_bytes") => {
            let bytes = byte_vec(args.first()?)?;
            Ok(if bytes.len() == 32 {
                Value::tag("Ok", [make_digest(bytes)])
            } else {
                Value::tag("Err", [Value::tag("WrongLength", [Value::record(vec![
                    ("expected", Value::Int(32)),
                    ("actual", Value::Int(bytes.len() as i128)),
                ])])])
            })
        }
        (_, "from_hex") => {
            let Value::Str(text) = args.first()? else { return None };
            // 64 hex digits for a 32-byte digest; a wrong count is `WrongLength`, an
            // out-of-range digit is `InvalidHex` with its index and byte value.
            if text.len() != 64 {
                return Some(Ok(Value::tag("Err", [Value::tag("WrongLength", [Value::record(vec![
                    ("expected", Value::Int(64)),
                    ("actual", Value::Int(text.len() as i128)),
                ])])])));
            }
            for (i, b) in text.bytes().enumerate() {
                if !(b as char).is_ascii_hexdigit() {
                    return Some(Ok(Value::tag("Err", [Value::tag("InvalidHex", [Value::record(vec![
                        ("index", Value::Int(i as i128)),
                        ("byte", Value::Int(i128::from(b))),
                    ])])])));
                }
            }
            Ok(Value::tag("Ok", [make_digest(crypto::from_hex(text)?)]))
        }
        _ => return None,
    })
}

/// A `List(U8)` value as bytes.
fn byte_vec(value: &Value) -> Option<Vec<u8>> {
    Some(value.sequence()?.iter().map(|v| match v {
        Value::Int(n) => *n as u8,
        Value::Dec(d) => (d / DEC_SCALE) as u8,
        _ => 0,
    }).collect())
}

/// Methods the interpreter can apply to ANY value structurally, so a runtime dispatch
/// that finds only nominal definitions none of which fit the value falls back to the
/// builtin — a `Dict` hashing a record, tuple or tag key through `to_hash`.
pub fn has_structural_builtin(method: &str) -> bool {
    method == "to_hash"
}

fn call_hasher(
    module: &str,
    method: &str,
    args: &[Value],
) -> Option<Result<Value, EvalError>> {
    // `Hasher.write_*`: the hasher first, then the value. Every width mixes the same
    // way, because rocflight keeps one integer representation.
    if module == "Hasher" && method.starts_with("write") {
        let (hasher, value) = (args.first()?, args.get(1)?);
        return Some(match hash_bytes(value) {
            Some(bytes) => hasher_write(hasher, &bytes),
            None => Err(EvalError {
                message: format!("{}.{} cannot hash {}", module, method, value),
            }),
        });
    }
    // `x.to_hash(hasher)`: the value first, then the hasher — it is a method ON the
    // value. Declared per type in `Builtin.roc` and an intrinsic for every type whose
    // representation the compiler owns.
    if method == "to_hash" {
        let (value, hasher) = (args.first()?, args.get(1)?);
        return Some(match hash_bytes(value) {
            Some(bytes) => hasher_write(hasher, &bytes),
            None => Err(EvalError {
                message: format!("{}.to_hash cannot hash {}", module, value),
            }),
        });
    }
    None
}

/// Is `Module.name` a CONSTANT rather than a function?
///
/// `U64.highest` is a value, so a bare reference to it has to be evaluated where it
/// stands. Left as a builtin it became the function value `<builtin U64.highest/1>`,
/// and `U64.highest - 1` then failed with "Invalid operands".
pub fn is_numeric_constant(module: &str, name: &str) -> bool {
    match module {
        "F32" | "F64" => {
            matches!(name, "highest" | "lowest" | "nan" | "infinity" | "e" | "pi" | "tau")
        }
        "Dec" => matches!(name, "highest" | "lowest" | "e" | "pi" | "tau"),
        _ => numeric_width(module).is_some() && matches!(name, "highest" | "lowest"),
    }
}

/// The bit width of a numeric module, and whether it is signed.
///
/// `Num` is not here: it is the shared namespace, not a width.
fn numeric_width(module: &str) -> Option<(u32, bool)> {
    Some(match module {
        "U8" => (8, false),
        "U16" => (16, false),
        "U32" => (32, false),
        "U64" => (64, false),
        "U128" => (128, false),
        "I8" => (8, true),
        "I16" => (16, true),
        "I32" => (32, true),
        "I64" => (64, true),
        "I128" => (128, true),
        _ => return None,
    })
}

/// Truncate a value to `bits`, interpreting the result as signed or unsigned.
///
/// This is what every `_wrap` operation in `Builtin.roc` means: keep the low bits and
/// let the rest go, which is how `U8.shl_wrap(200, 1)` is 144 rather than an overflow.
fn wrap_to(value: i128, bits: u32, signed: bool) -> i128 {
    if bits >= 128 {
        return value;
    }
    let truncated = value & ((1i128 << bits) - 1);
    if signed && (truncated >> (bits - 1)) & 1 == 1 {
        truncated - (1i128 << bits)
    } else {
        truncated
    }
}

/// How a width conversion treats a value that does not fit.
#[derive(Clone, Copy, PartialEq)]
enum Conversion {
    /// `to_u64`: written only where the value is known to fit.
    Plain,
    /// `to_u8_wrap`: keep the low bits.
    Wrap,
    /// `to_i8_try`: `Ok(value)` or `Err(OutOfRange)`.
    Try,
}

/// `to_u32_wrap`, `to_i8_try`, `to_u64` — the target width is in the NAME.
fn conversion_target(method: &str) -> Option<(u32, bool, Conversion)> {
    let (rest, mode) = if let Some(rest) = method.strip_suffix("_wrap") {
        (rest, Conversion::Wrap)
    } else if let Some(rest) = method.strip_suffix("_try") {
        (rest, Conversion::Try)
    } else {
        (method, Conversion::Plain)
    };
    let rest = rest.strip_prefix("to_")?;
    let signed = match rest.as_bytes().first()? {
        b'u' => false,
        b'i' => true,
        _ => return None,
    };
    let bits: u32 = rest[1..].parse().ok()?;
    matches!(bits, 8 | 16 | 32 | 64 | 128).then_some((bits, signed, mode))
}

/// A width as one byte for the VM's `BinInt`: `bits | (signed << 7)`, 0 for none.
pub fn width_code(module: &str) -> u8 {
    match numeric_width(module) {
        // 128-bit does not fit the `bits | sign` byte, so it gets sentinels: the
        // `BinInt` opcode reads these and crashes on i128 overflow for `I128`.
        Some((128, true)) => I128_WIDTH,
        Some((128, false)) | None => 0,
        Some((bits, signed)) => bits as u8 | if signed { 0x80 } else { 0 },
    }
}

/// The `width_code` sentinel for `I128`; `BinInt` treats it as "check i128 overflow".
pub const I128_WIDTH: u8 = 0xFF;

/// Does `value` fit the width `width_code` encoded? On every integer operator the
/// VM runs, so two shifts rather than a bounds computation.
#[inline]
pub fn fits_width(value: i128, code: u8) -> bool {
    let bits = u32::from(code & 0x7F);
    if code & 0x80 != 0 {
        let spare = 128 - bits;
        (value << spare) >> spare == value
    } else {
        value >= 0 && value >> bits == 0
    }
}

/// The lowest and highest value of a width.
fn width_bounds(bits: u32, signed: bool) -> (i128, i128) {
    match (bits, signed) {
        (128, true) => (i128::MIN, i128::MAX),
        // `U128.highest` is above `i128::MAX`; the i128 representation caps it there,
        // which is the one width this interpreter cannot hold in full.
        (128, false) => (0, i128::MAX),
        (_, true) => (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1),
        (_, false) => (0, (1i128 << bits) - 1),
    }
}

/// The checked and saturating arithmetic every numeric width declares, plus `Dec`.
///
/// `plus_try : a, a -> Try(a, [Overflow, ..])` and its siblings are how roc writes
/// arithmetic that must not wrap. `Dec` has no fixed width here — it is an i128 of
/// eighteen decimal places — so its bound is the i128's.
fn call_checked(
    module: &str,
    method: &str,
    args: &[Value],
) -> Option<Result<Value, EvalError>> {
    let is_dec = module == "Dec" || args.iter().any(|a| matches!(a, Value::Dec(_)));
    if !is_dec && numeric_width(module).is_none() {
        return None;
    }

    // `Dec.lowest` and `Dec.highest` are the bounds of the underlying i128.
    if is_dec {
        match method {
            "highest" => return Some(Ok(Value::Dec(i128::MAX))),
            "lowest" => return Some(Ok(Value::Dec(i128::MIN))),
            "e" => return Some(Ok(Value::Dec(dec_from_str("2.718281828459045235").expect("literal")))),
            "pi" => return Some(Ok(Value::Dec(dec_from_str("3.141592653589793238").expect("literal")))),
            "tau" => return Some(Ok(Value::Dec(dec_from_str("6.283185307179586476").expect("literal")))),
            "from_attos" => return Some(Ok(Value::Dec(as_whole(args.first()?)?))),
            _ => {}
        }
    }
    // `n.to_dec()` widens a whole number into a fixed-point one.
    if method == "to_dec" {
        return Some(match as_dec(args.first()?) {
            Some(raw) => Ok(Value::Dec(raw)),
            None => Err(EvalError {
                message: format!("to_dec needs a number, got {}", args[0]),
            }),
        });
    }
    if !is_dec {
        return None;
    }
    if let Some(result) = call_dec(method, args) {
        return Some(result);
    }

    let (a, b) = (as_dec(args.first()?)?, as_dec(args.get(1)?)?);
    let op = match method {
        "plus_try" | "plus_saturated" => BinOp::Add,
        "minus_try" | "minus_saturated" => BinOp::Sub,
        "times_try" | "times_saturated" => BinOp::Mul,
        "div_by" => BinOp::Div,
        _ => return None,
    };
    let exact = match op {
        BinOp::Add => a.checked_add(b),
        BinOp::Sub => a.checked_sub(b),
        // The product carries two factors of the scale; one comes back out.
        BinOp::Mul => dec_mul(a, b),
        _ => None,
    };
    Some(Ok(if method.ends_with("_try") {
        // `Try` on overflow, which is what lets `SafeMath` report one.
        match exact {
            Some(value) => Value::tag("Ok", [Value::Dec(value)]),
            None => Value::tag("Err", [Value::tag("Overflow", Vec::new())]),
        }
    } else {
        // Saturating: pinned at the bound, which is how `times_saturated` signals an
        // overflow to a caller that then compares against `Dec.highest`.
        match exact {
            Some(value) => Value::Dec(value),
            None => {
                // Which bound depends on the direction of the overflow.
                let positive = match op {
                    BinOp::Add => b >= 0,
                    BinOp::Sub => b < 0,
                    _ => (a < 0) == (b < 0),
                };
                Value::Dec(if positive { i128::MAX } else { i128::MIN })
            }
        }
    }))
}

/// `Dec`'s own methods beyond the checked arithmetic: the conversions out of fixed
/// point, rounding, and the functions that go through an f64 and come back.
fn call_dec(method: &str, args: &[Value]) -> Option<Result<Value, EvalError>> {
    let a = as_dec(args.first()?)?;
    let crash = |what: &str| Err(EvalError { message: format!("crash: Dec.{} {}", method, what) });
    // The whole part, truncated toward zero — what every `to_<int>` conversion starts
    // from — and the nearest whole, halfway away from zero, for the `round_to_` ones.
    let truncated = a / DEC_SCALE;
    // Half away from zero, without `abs` — `Dec.highest` plus half a unit overflows.
    let rounded = if (a % DEC_SCALE).abs() * 2 >= DEC_SCALE { truncated + a.signum() } else { truncated };
    let floored = a.div_euclid(DEC_SCALE);
    let ceiled = -(a.checked_neg().unwrap_or(i128::MAX).div_euclid(DEC_SCALE));
    let via_f64 = |f: fn(f64) -> f64| Ok(Value::Dec(dec_from_f64(f(a as f64 / DEC_SCALE as f64))));
    // Correctly rounded, once: through the decimal digits, not through an f64.
    let as_f64 = || dec_to_string(a).parse::<f64>().unwrap_or(0.0);
    let as_f32 = || dec_to_string(a).parse::<f32>().unwrap_or(0.0);
    let unary = match method {
        m if m.starts_with("range_") && args.len() == 2 => {
            let step = Value::Dec(DEC_SCALE);
            return numeric_range(m, Value::Dec(a), args[1].clone(), step);
        }
        "to_attos" => Some(Ok(Value::Int(a))),
        "abs" => Some(match a.checked_abs() {
            Some(v) => Ok(Value::Dec(v)),
            None => crash("overflowed"),
        }),
        "negate" => Some(Ok(Value::Dec(-a))),
        "is_zero" => Some(Ok(Value::Bool(a == 0))),
        "is_negative" => Some(Ok(Value::Bool(a < 0))),
        "is_positive" => Some(Ok(Value::Bool(a > 0))),
        "to_f64" => Some(Ok(Value::Float(as_f64()))),
        "to_f32" | "to_f32_wrap" => Some(Ok(Value::F32(as_f32()))),
        "to_f32_try" => Some(Ok(Value::tag("Ok", [Value::F32(as_f32())]))),
        "round" => Some(Ok(Value::Dec(rounded * DEC_SCALE))),
        "floor" => Some(Ok(Value::Dec(floored * DEC_SCALE))),
        "ceiling" => Some(Ok(Value::Dec(ceiled * DEC_SCALE))),
        "round_to_i128" => Some(Ok(Value::Int(rounded))),
        "sqrt" if a < 0 => Some(crash("of a negative")),
        "sqrt" => Some(via_f64(f64::sqrt)),
        "sqrt_try" if a < 0 => Some(Ok(Value::tag("Err", [Value::bare("SqrtOfNegative")]))),
        "sqrt_try" => Some(via_f64(f64::sqrt).map(|v| Value::tag("Ok", [v]))),
        "sin" => Some(via_f64(f64::sin)),
        "cos" => Some(via_f64(f64::cos)),
        "tan" => Some(via_f64(f64::tan)),
        "asin" | "acos" if a.abs() > DEC_SCALE => Some(crash("outside -1.0 through 1.0")),
        "asin" => Some(via_f64(f64::asin)),
        "acos" => Some(via_f64(f64::acos)),
        "atan" => Some(via_f64(f64::atan)),
        _ => None,
    };
    if let Some(answer) = unary {
        return Some(answer);
    }
    // `to_i8_try`, `round_to_i32_try`, `to_u64_wrap`: the whole to convert, then the
    // target width off the name.
    let (whole, rest) = if let Some(rest) = method.strip_prefix("round_") {
        (rounded, rest)
    } else if let Some(rest) = method.strip_prefix("floor_") {
        (floored, rest)
    } else if let Some(rest) = method.strip_prefix("ceiling_") {
        (ceiled, rest)
    } else {
        (truncated, method)
    };
    if let Some((bits, signed, mode)) = conversion_target(rest) {
        let wrapped = wrap_to(whole, bits, signed);
        return Some(match mode {
            Conversion::Wrap => Ok(Value::Int(wrapped)),
            Conversion::Try if wrapped == whole => Ok(Value::tag("Ok", [Value::Int(whole)])),
            Conversion::Try => Ok(Value::tag("Err", [Value::bare("OutOfRange")])),
            Conversion::Plain if wrapped == whole => Ok(Value::Int(whole)),
            Conversion::Plain => crash("does not fit"),
        });
    }
    let b = as_dec(args.get(1)?)?;
    Some(match method {
        "div_by" if b == 0 => crash("divided by zero"),
        "div_by" => match dec_div(a, b) {
            Some(v) => Ok(Value::Dec(v)),
            None => crash("overflowed"),
        },
        "div_floor_by" if b == 0 => crash("divided by zero"),
        "div_floor_by" => {
            let q = a / b - i128::from((a % b != 0) && ((a < 0) != (b < 0)));
            Ok(Value::Dec(q * DEC_SCALE))
        }
        "order_relative_to" => Ok(Value::tag(
            match a.cmp(&b) {
                std::cmp::Ordering::Less => "Before",
                std::cmp::Ordering::Equal => "Same",
                std::cmp::Ordering::Greater => "After",
            },
            vec![],
        )),
        "is_eq" => Ok(Value::Bool(a == b)),
        "is_ne" => Ok(Value::Bool(a != b)),
        "is_lt" => Ok(Value::Bool(a < b)),
        "is_lte" => Ok(Value::Bool(a <= b)),
        "is_gt" => Ok(Value::Bool(a > b)),
        "is_gte" => Ok(Value::Bool(a >= b)),
        "min" => Ok(Value::Dec(a.min(b))),
        "max" => Ok(Value::Dec(a.max(b))),
        "abs_diff" => match a.checked_sub(b).and_then(i128::checked_abs) {
            Some(v) => Ok(Value::Dec(v)),
            None => crash("overflowed"),
        },
        // The arithmetic operators as method names: `x.plus(y)` is `x + y`, through
        // the same overflow-checked `Dec` arithmetic the `+` operator uses.
        "plus" | "minus" | "times" => {
            let op = match method {
                "plus" => BinOp::Add, "minus" => BinOp::Sub, _ => BinOp::Mul,
            };
            return dec_binop(op, a, b);
        }
        "rem_by" if b == 0 => crash("divided by zero"),
        "rem_by" => Ok(Value::Dec(a % b)),
        "div_trunc_by" if b == 0 => crash("divided by zero"),
        "div_trunc_by" => Ok(Value::Dec((a / b) * DEC_SCALE)),
        "pow" => {
            let (x, y) = (a as f64 / DEC_SCALE as f64, b as f64 / DEC_SCALE as f64);
            let r = x.powf(y);
            if r.is_nan() || r.is_infinite() { crash("is undefined here") } else { Ok(Value::Dec(dec_from_f64(r))) }
        }
        _ => return None,
    })
}

/// The float methods, for both widths. `F32` answers are rounded to an f32.
fn call_float(module: &str, method: &str, args: &[Value]) -> Option<Result<Value, EvalError>> {
    let narrow = module == "F32";
    if !narrow && module != "F64" {
        return None;
    }
    let mk = |x: f64| if narrow { Value::F32(x as f32) } else { Value::Float(x) };
    let ok = |x: f64| Ok(mk(x));
    match method {
        "highest" => return Some(ok(if narrow { f64::from(f32::MAX) } else { f64::MAX })),
        "lowest" => return Some(ok(if narrow { f64::from(f32::MIN) } else { f64::MIN })),
        "nan" => return Some(ok(f64::NAN)),
        "infinity" => return Some(ok(f64::INFINITY)),
        "e" => return Some(ok(std::f64::consts::E)),
        "pi" => return Some(ok(std::f64::consts::PI)),
        "tau" => return Some(ok(std::f64::consts::TAU)),
        m if m.starts_with("range_") && args.len() == 2 => {
            let step = if narrow { Value::F32(1.0) } else { Value::Float(1.0) };
            return numeric_range(m, args[0].clone(), args[1].clone(), step);
        }
        "from_str" => {
            let Value::Str(text) = args.first()? else { return None };
            let text = text.trim();
            // Rust reads `inf` and `NaN`; roc's `from_str` reads digits.
            let parsed = if text.bytes().any(|b| b.is_ascii_alphabetic() && b != b'e' && b != b'E') {
                None
            } else {
                // `"1e400"` is not a number an `F64` can hold.
                text.parse::<f64>().ok().filter(|x| x.is_finite())
            };
            return Some(Ok(match parsed {
                Some(x) => Value::tag("Ok", [mk(x)]),
                None => Value::tag("Err", [Value::bare("BadNumStr")]),
            }));
        }
        "from_bits" => {
            let bits = as_whole(args.first()?)?;
            return Some(ok(if narrow {
                f64::from(f32::from_bits(bits as u32))
            } else {
                f64::from_bits(bits as u64)
            }));
        }
        _ => {}
    }
    let a = as_f64(args.first()?)?;
    let crash = |what: &str| Err(EvalError { message: format!("crash: {}.{} {}", module, method, what) });
    let unary = match method {
        // Every NaN has the same bits here, as roc collapses them.
        "to_bits" => Some(Ok(Value::Int(match (narrow, a.is_nan()) {
            (true, true) => 0x7FC0_0000,
            (true, false) => i128::from((a as f32).to_bits()),
            (false, true) => 0x7FF8_0000_0000_0000,
            (false, false) => i128::from(a.to_bits()),
        }))),
        "to_str" => Some(Ok(str_value(mk(a).to_string()))),
        "abs" => Some(ok(a.abs())),
        "negate" => Some(ok(-a)),
        "sqrt" => Some(ok(a.sqrt())),
        "sqrt_try" => Some(Ok(if a < 0.0 {
            Value::tag("Err", [Value::bare("SqrtOfNegative")])
        } else {
            Value::tag("Ok", [mk(a.sqrt())])
        })),
        // Bit-exact with roc: its F64 transcendentals are musl's (the `libm` crate
        // ports the same code) and its F32 ones are its own binary32 algorithms.
        "sin" => Some(ok(if narrow { f32math::sin(a as f32).into() } else { libm::sin(a) })),
        "cos" => Some(ok(if narrow { f32math::cos(a as f32).into() } else { libm::cos(a) })),
        "tan" => Some(ok(if narrow { f32math::tan(a as f32).into() } else { libm::tan(a) })),
        "asin" => Some(ok(if narrow { f32math::asin(a as f32).into() } else { libm::asin(a) })),
        "acos" => Some(ok(if narrow { f32math::acos(a as f32).into() } else { libm::acos(a) })),
        "atan" => Some(ok(if narrow { f32math::atan(a as f32).into() } else { libm::atan(a) })),
        "log" => Some(ok(a.ln())),
        "exp" => Some(ok(a.exp())),
        "round" => Some(ok(a.round())),
        "floor" => Some(ok(a.floor())),
        "ceiling" => Some(ok(a.ceil())),
        "is_nan" => Some(Ok(Value::Bool(a.is_nan()))),
        "is_infinite" => Some(Ok(Value::Bool(a.is_infinite()))),
        "is_finite" => Some(Ok(Value::Bool(a.is_finite()))),
        "is_zero" => Some(Ok(Value::Bool(a == 0.0))),
        "is_negative" => Some(Ok(Value::Bool(a < 0.0))),
        "is_positive" => Some(Ok(Value::Bool(a > 0.0))),
        "to_f64" => Some(Ok(Value::Float(a))),
        "to_f32" | "to_f32_wrap" => Some(Ok(Value::F32(a as f32))),
        // Past `F32.highest` is out of range even where rounding would land on it.
        "to_f32_try" => Some(Ok(if !a.is_finite() || a.abs() > f64::from(f32::MAX) {
            Value::tag("Err", [Value::bare("OutOfRange")])
        } else {
            Value::tag("Ok", [Value::F32(a as f32)])
        })),
        "to_dec" => Some(Ok(Value::Dec(dec_from_f64(a)))),
        _ => None,
    };
    if let Some(answer) = unary {
        return Some(answer);
    }
    // `to_i64_try`, `round_to_i32_try`, `to_u8_wrap`: a whole number, then the width.
    let (whole, rest) = if let Some(rest) = method.strip_prefix("round_") {
        (a.round(), rest)
    } else if let Some(rest) = method.strip_prefix("floor_") {
        (a.floor(), rest)
    } else if let Some(rest) = method.strip_prefix("ceiling_") {
        (a.ceil(), rest)
    } else {
        (a.trunc(), method)
    };
    if let Some((bits, signed, mode)) = conversion_target(rest) {
        // `i128::MAX` as an f64 is 2^127, which is one past the top: read it as "too
        // big" rather than letting the cast saturate. Wrapping reads the value modulo
        // 2^128 first, which an f64 can do exactly, so `to_i128_wrap(2^127)` is the
        // lowest I128 and not zero.
        let representable = whole.is_finite() && whole.abs() < 2f64.powi(127);
        let as_int = if representable { Some(whole as i128) } else { None };
        let wrapped_bits: i128 = if !whole.is_finite() {
            0
        } else if whole.abs() < 2f64.powi(127) {
            whole as i128
        } else if whole.abs() < 2f64.powi(128) {
            // Exact: a float this large is a whole number a u128 holds.
            let magnitude = whole.abs() as u128;
            (if whole < 0.0 { 0u128.wrapping_sub(magnitude) } else { magnitude }) as i128
        } else {
            0
        };
        return Some(match mode {
            Conversion::Wrap => Ok(Value::Int(wrap_to(wrapped_bits, bits, signed))),
            Conversion::Try | Conversion::Plain => {
                let fits = as_int.filter(|n| wrap_to(*n, bits, signed) == *n);
                match (fits, mode) {
                    (Some(n), Conversion::Try) => Ok(Value::tag("Ok", [Value::Int(n)])),
                    (None, Conversion::Try) => Ok(Value::tag("Err", [Value::bare("OutOfRange")])),
                    (Some(n), _) => Ok(Value::Int(n)),
                    (None, _) => crash("does not fit"),
                }
            }
        });
    }
    let b = as_f64(args.get(1)?)?;
    Some(match method {
        "plus" => ok(a + b),
        "minus" => ok(a - b),
        "times" => ok(a * b),
        "div_by" => ok(a / b),
        // The sign follows the dividend, as Rust's `%` does.
        "rem_by" => ok(a % b),
        "div_trunc_by" => ok((a / b).trunc()),
        "div_floor_by" => ok((a / b).floor()),
        "div_ceil_by" => ok((a / b).ceil()),
        "pow" => ok(if narrow { libm::powf(a as f32, b as f32).into() } else { libm::pow(a, b) }),
        "min" => ok(a.min(b)),
        "max" => ok(a.max(b)),
        "abs_diff" => ok((a - b).abs()),
        "is_eq" | "is_float_eq" => Ok(Value::Bool(a == b)),
        "is_ne" => Ok(Value::Bool(a != b)),
        "is_lt" => Ok(Value::Bool(a < b)),
        "is_lte" => Ok(Value::Bool(a <= b)),
        "is_gt" => Ok(Value::Bool(a > b)),
        "is_gte" => Ok(Value::Bool(a >= b)),
        "order_relative_to" => Ok(Value::tag(
            match a.partial_cmp(&b) {
                Some(std::cmp::Ordering::Less) => "Before",
                Some(std::cmp::Ordering::Greater) => "After",
                _ => "Same",
            },
            vec![],
        )),
        _ => return None,
    })
}

/// A range built by `range_*_to` / `range_*_from` over a non-integer type: a lazy
/// iterator, since a `Dec` or float range is walked, not indexed. `from` reverses.
fn numeric_range(method: &str, receiver: Value, other: Value, step: Value) -> Option<Result<Value, EvalError>> {
    let inclusive = method.contains("inclusive");
    let from = method.ends_with("_from");
    // `n.range_*_to(m)`: receiver is the lower bound. `n.range_*_from(m)`: receiver is
    // the upper, `m` the lower, and the elements come out in reverse.
    let (lo, hi) = if from { (other, receiver) } else { (receiver, other) };
    let range = Value::Iter(std::rc::Rc::new(lazy::Lazy::Range {
        at: lo, end: hi, step, inclusive,
    }));
    if !from {
        return Some(Ok(range));
    }
    // `from` reverses the ascending elements: a range walking DOWN from the last of
    // them, so a later `step_by` can re-anchor at the lower bound — roc's
    // `5.Dec.range_inclusive_from(1).step_by(1.5)` is `[4.0, 2.5, 1.0]`.
    let Value::Iter(l) = &range else { unreachable!() };
    let ascending = match lazy::materialize(l) {
        Ok(items) => items,
        Err(e) => return Some(Err(e)),
    };
    let Some(top) = ascending.last() else { return Some(Ok(Value::list(Vec::new()))) };
    let (lo, step) = match &**l {
        lazy::Lazy::Range { at, step, .. } => (at.clone(), step.clone()),
        _ => unreachable!(),
    };
    let down = apply_binop(BinOp::Sub, &step, &step).and_then(|zero| apply_binop(BinOp::Sub, &zero, &step));
    Some(down.map(|down| {
        Value::Iter(std::rc::Rc::new(lazy::Lazy::Range { at: top.clone(), end: lo, step: down, inclusive: true }))
    }))
}

/// The numeric operations `Builtin.roc` writes as methods on a width.
///
/// Bit manipulation, wrapping shifts and width conversions — the ops a `Dict`'s bucket
/// arithmetic is built from. They are dispatched on the module, which names the width,
/// so `U32.shl_wrap(1, 8)` truncates to 32 bits and `U8.shl_wrap(1, 8)` is 0.
///
/// One `i128` holds every width in full except `U128` above `i128::MAX`.
fn call_numeric(
    module: &str,
    method: &str,
    args: &[Value],
) -> Option<Result<Value, EvalError>> {
    let (bits, signed) = numeric_width(module)?;
    let whole = as_whole;
    let out = |v: i128| Ok(Value::Int(wrap_to(v, bits, signed) as i128));

    let (lo, hi) = width_bounds(bits, signed);
    let fits = |v: i128| v >= lo && v <= hi;
    // A wrong answer roc reports by crashing: `crash:` is the prefix the harness reads
    // as one, so a crash test sees a crash here rather than a wrong value.
    let crash = |what: &str| Err(EvalError { message: format!("crash: {}.{} {}", module, method, what) });

    // `U32.highest`, `I8.lowest` — the bounds of the width, as values rather than calls.
    match method {
        "highest" => return Some(Ok(Value::Int(hi))),
        "lowest" => return Some(Ok(Value::Int(lo))),
        // `U8.from_str("256")` is `Err(BadNumStr)`: the width decides, not the parse.
        "from_str" => {
            let Value::Str(text) = args.first()? else { return None };
            // Plain digits first; then the exact decimal reader, so that `"2e5"` and
            // `"1.0"` are whole numbers too.
            let text = text.trim();
            let parsed = text.parse::<i128>().ok().or_else(|| {
                let attos = dec_from_str(text)?;
                (attos % DEC_SCALE == 0).then_some(attos / DEC_SCALE)
            });
            return Some(Ok(match parsed {
                Some(n) if fits(n) => Value::tag("Ok", [Value::Int(n)]),
                _ => Value::tag("Err", [Value::bare("BadNumStr")]),
            }));
        }
        _ => {}
    }

    let a = whole(args.first()?)?;
    // A conversion reads its TARGET width off the method name, not the receiver's.
    if let Some((to_bits, to_signed, mode)) = conversion_target(method) {
        let converted = wrap_to(a, to_bits, to_signed);
        return Some(match mode {
            Conversion::Wrap => Ok(Value::Int(converted)),
            Conversion::Try if converted == a => Ok(Value::tag("Ok", [Value::Int(a)])),
            Conversion::Try => Ok(Value::tag("Err", [Value::bare("OutOfRange")])),
            Conversion::Plain if converted == a => Ok(Value::Int(a)),
            Conversion::Plain => Err(EvalError {
                message: format!("{}.{} cannot hold {}", module, method, a),
            }),
        });
    }

    // The value read as its width's unsigned bit pattern, for the bit counts.
    let unsigned = wrap_to(a, bits, false) as u128;
    let b = args.get(1).and_then(whole);
    let try_of = |exact: Option<i128>| {
        Ok(match exact {
            Some(v) if fits(v) => Value::tag("Ok", [Value::Int(v)]),
            _ => Value::tag("Err", [Value::bare("Overflow")]),
        })
    };
    let saturated = |exact: Option<i128>, positive: bool| {
        Ok(Value::Int(match exact {
            Some(v) => v.clamp(lo, hi),
            None if positive => hi,
            None => lo,
        }))
    };
    let checked = |exact: Option<i128>| match exact {
        Some(v) if fits(v) => Ok(Value::Int(v)),
        _ => crash("overflowed"),
    };
    Some(match (method, b) {
        ("count_one_bits", _) => Ok(Value::Int(i128::from(unsigned.count_ones()))),
        ("count_leading_zero_bits", _) => {
            Ok(Value::Int(i128::from(unsigned.leading_zeros() - (128 - bits))))
        }
        ("count_trailing_zero_bits", _) => {
            Ok(Value::Int(i128::from(unsigned.trailing_zeros().min(bits))))
        }
        ("abs", _) => checked(a.checked_abs()),
        // `I8.negate(I8.lowest)` overflows the width — its magnitude is one past the
        // top — and roc crashes; `checked` reports that.
        ("negate", _) => checked(a.checked_neg()),
        ("is_zero", _) => Ok(Value::Bool(a == 0)),
        ("is_even", _) => Ok(Value::Bool(a % 2 == 0)),
        ("is_odd", _) => Ok(Value::Bool(a % 2 != 0)),
        ("is_negative", _) => Ok(Value::Bool(a < 0)),
        ("is_positive", _) => Ok(Value::Bool(a > 0)),
        ("bitwise_and", Some(b)) => out(a & b),
        ("bitwise_or", Some(b)) => out(a | b),
        ("bitwise_xor", Some(b)) => out(a ^ b),
        ("bitwise_not", _) => out(!a),
        ("shl_wrap", Some(n)) => out(a << (n as u32 % bits)),
        // Arithmetic: the sign bit is carried down, which is what makes
        // `I8.shr_wrap(x, 7)` the all-ones mask a seed check wants.
        ("shr_wrap", Some(n)) => out(a >> (n as u32 % bits)),
        // Zero-fill: the value is read as unsigned first, so nothing is carried down.
        ("shr_zf_wrap", Some(n)) => out((unsigned >> (n as u32 % bits)) as i128),
        ("is_eq", Some(b)) => Ok(Value::Bool(a == b)),
        ("is_ne", Some(b)) => Ok(Value::Bool(a != b)),
        ("is_lt", Some(b)) => Ok(Value::Bool(a < b)),
        ("is_lte", Some(b)) => Ok(Value::Bool(a <= b)),
        ("is_gt", Some(b)) => Ok(Value::Bool(a > b)),
        ("is_gte", Some(b)) => Ok(Value::Bool(a >= b)),
        ("order_relative_to", Some(b)) => Ok(Value::tag(
            match a.cmp(&b) {
                std::cmp::Ordering::Less => "Before",
                std::cmp::Ordering::Equal => "Same",
                std::cmp::Ordering::Greater => "After",
            },
            vec![],
        )),
        ("min", Some(b)) => Ok(Value::Int(a.min(b))),
        ("max", Some(b)) => Ok(Value::Int(a.max(b))),
        ("abs_diff", Some(b)) => Ok(Value::Int((a - b).abs())),
        ("pow", Some(b)) => checked(u32::try_from(b).ok().and_then(|e| a.checked_pow(e))),
        // A negative exponent on a whole number underflows to a fraction — except a
        // base of 1 (always 1) or -1 (±1 by the exponent's parity), which are exact.
        ("pow_try", Some(b)) if b < 0 && a == 1 => Ok(Value::tag("Ok", [Value::Int(1)])),
        ("pow_try", Some(b)) if b < 0 && a == -1 => {
            Ok(Value::tag("Ok", [Value::Int(if b % 2 == 0 { 1 } else { -1 })]))
        }
        ("pow_try", Some(b)) if b < 0 => Ok(Value::tag("Err", [Value::bare("Underflow")])),
        ("pow_try", Some(b)) => try_of(u32::try_from(b).ok().and_then(|e| a.checked_pow(e))),
        ("plus", Some(b)) => checked(a.checked_add(b)),
        ("minus", Some(b)) => checked(a.checked_sub(b)),
        ("times", Some(b)) => checked(a.checked_mul(b)),
        ("plus_wrap", Some(b)) => out(a.wrapping_add(b)),
        ("minus_wrap", Some(b)) => out(a.wrapping_sub(b)),
        ("times_wrap", Some(b)) => out(a.wrapping_mul(b)),
        ("plus_overflows", Some(b)) => Ok(Value::Bool(!a.checked_add(b).is_some_and(fits))),
        ("minus_overflows", Some(b)) => Ok(Value::Bool(!a.checked_sub(b).is_some_and(fits))),
        ("times_overflows", Some(b)) => Ok(Value::Bool(!a.checked_mul(b).is_some_and(fits))),
        ("plus_try", Some(b)) => try_of(a.checked_add(b)),
        ("minus_try", Some(b)) => try_of(a.checked_sub(b)),
        ("times_try", Some(b)) => try_of(a.checked_mul(b)),
        ("plus_saturated", Some(b)) => saturated(a.checked_add(b), b >= 0),
        ("minus_saturated", Some(b)) => saturated(a.checked_sub(b), b < 0),
        ("times_saturated", Some(b)) => saturated(a.checked_mul(b), (a < 0) == (b < 0)),
        ("div_by", Some(0)) | ("div_trunc_by", Some(0)) | ("rem_by", Some(0))
        | ("mod_by", Some(0)) | ("div_floor_by", Some(0)) | ("div_ceil_by", Some(0)) => {
            crash("divided by zero")
        }
        ("div_try", Some(0)) | ("div_ceil_try", Some(0)) | ("div_floor_try", Some(0)) => {
            Ok(Value::tag("Err", [Value::bare("DivByZero")]))
        }
        ("div_try", Some(b)) => try_of(a.checked_div(b)),
        ("div_ceil_try", Some(b)) => try_of(
            a.checked_div(b).map(|q| q + i128::from((a % b != 0) && ((a < 0) == (b < 0)))),
        ),
        ("div_floor_try", Some(b)) => try_of(
            a.checked_div(b).map(|q| q - i128::from((a % b != 0) && ((a < 0) != (b < 0)))),
        ),
        ("div_by", Some(b)) | ("div_trunc_by", Some(b)) => checked(a.checked_div(b)),
        ("rem_by", Some(b)) => out(a % b),
        // The sign follows the DIVISOR: `I8.mod_by(-7, 3)` is 2.
        ("mod_by", Some(b)) => out(((a % b) + b) % b),
        // Toward negative infinity, whatever the signs: `I8.div_floor_by(7, -2)` is -4.
        ("div_floor_by", Some(b)) => out(a / b - i128::from((a % b != 0) && ((a < 0) != (b < 0)))),
        ("div_ceil_by", Some(b)) => out(a / b + i128::from((a % b != 0) && ((a < 0) == (b < 0)))),
        ("range_exclusive_to", Some(b)) => Ok(Value::Range { start: a, end: b, inclusive: false, step: 1 }),
        ("range_inclusive_to", Some(b)) => Ok(Value::Range { start: a, end: b, inclusive: true, step: 1 }),
        // `n.range_*_from(m)`: the receiver is the UPPER bound and the elements come
        // out in reverse — `5.range_exclusive_from(1)` is `[4, 3, 2, 1]`.
        (m @ ("range_exclusive_from" | "range_inclusive_from"), Some(b)) => {
            let inclusive = m == "range_inclusive_from";
            let last = if inclusive { a } else { a - 1 };
            Ok(Value::list((b..=last).rev().map(Value::Int).collect()))
        }
        _ => return None,
    })
}

/// The LOW-LEVEL ops `Builtin.roc` calls but never defines.
///
/// In the real compiler these are `LowLevel` variants that `canonicalize/
/// BuiltinLowLevel.zig` rewrites the annotation-only declarations into; the list of
/// them is `src/base/LowLevel.zig`, 502 long. rocflight keeps one integer
/// representation and one float, so the per-width families collapse and only these are
/// needed. The names are exactly as Builtin.roc spells them — `rocflight
/// --builtins=names` lists every one still missing.
///
/// This is also the registry: the compiler asks here whether a bare name is a
/// low-level op, so the list and the implementations cannot drift apart.
pub fn low_level_arity(name: &str) -> Option<usize> {
    Some(match name {
        "list_get_unsafe" => 2,
        "list_set_unsafe" => 3,
        "list_swap_unsafe" => 3,
        "list_append_unsafe" | "u8_list_append_unsafe" => 2,
        "list_replace_unsafe" => 3,
        "list_with_capacity" | "u8_list_with_capacity" => 1,
        "u8_list_len" => 1,
        "u8_list_get_unsafe" => 2,
        "hasher_finish" => 1,
        "dict_pseudo_seed" => 0,
        _ => return None,
    })
}

/// Run one low-level op. `low_level_arity` decides what reaches here.
fn call_low_level(name: &str, args: &mut [Value]) -> Result<Value, EvalError> {
    let wrong = |what: &str| EvalError { message: format!("{} needs {}", name, what) };
    // Moved out of `args`, so a list nothing else holds is mutated in place — which is
    // what `Dict`'s bucket writes count on being cheap.
    let list = |v: &mut Value| match std::mem::replace(v, Value::Unit) {
        Value::List(items) => Ok(value::into_items(items)),
        other => Err(EvalError { message: format!("{} needs a List, got {}", name, other) }),
    };
    let index = |v: &Value| match v {
        Value::Int(n) if *n >= 0 => Ok(*n as usize),
        other => Err(EvalError { message: format!("{} needs an index, got {}", name, other) }),
    };
    match name {
        // "unsafe" means the CALLER has already proved the index is in bounds. Roc
        // elides the check; rocflight cannot elide a Rust bounds check, so an
        // out-of-range index is a message rather than a panic.
        // A read: the list stays where it is, no copy.
        "list_get_unsafe" | "u8_list_get_unsafe" => {
            let i = index(&args[1])?;
            let Value::List(items) = &args[0] else {
                return Err(EvalError { message: format!("{} needs a List, got {}", name, args[0]) });
            };
            items.get(i).cloned().ok_or_else(|| EvalError {
                message: format!("{}: index {} is past the end of a list of {}", name, i, items.len()),
            })
        }
        "list_set_unsafe" => {
            let i = index(&args[1])?;
            let mut items = list(&mut args[0])?;
            *items.get_mut(i).ok_or_else(|| wrong("an index within the list"))? = args[2].clone();
            Ok(Value::list(items))
        }
        "list_swap_unsafe" => {
            let (i, j) = (index(&args[1])?, index(&args[2])?);
            let mut items = list(&mut args[0])?;
            if i >= items.len() || j >= items.len() {
                return Err(wrong("two indices within the list"));
            }
            items.swap(i, j);
            Ok(Value::list(items))
        }
        "list_append_unsafe" | "u8_list_append_unsafe" => {
            let mut items = list(&mut args[0])?;
            items.push(args[1].clone());
            Ok(Value::list(items))
        }
        // Returns the new list paired with what was there, so a caller can reuse the
        // displaced value without a second lookup.
        "list_replace_unsafe" => {
            let i = index(&args[1])?;
            let mut items = list(&mut args[0])?;
            let slot = items.get_mut(i).ok_or_else(|| wrong("an index within the list"))?;
            let prev = std::mem::replace(slot, args[2].clone());
            Ok(Value::record(vec![("list", Value::list(items)), ("prev", prev)]))
        }
        // Capacity is a hint about allocation, which is not observable through the API.
        "list_with_capacity" | "u8_list_with_capacity" => Ok(Value::list(Vec::new())),
        "u8_list_len" => match &args[0] {
            Value::List(items) => Ok(Value::Int(items.len() as i128)),
            other => Err(EvalError { message: format!("{} needs a List, got {}", name, other) }),
        },
        // `Hasher :: { state : U64 }`, and the digest IS the state: every `write_*`
        // has already mixed into it.
        "hasher_finish" => match &args[0] {
            Value::Record(fields) => fields
                .iter()
                .find(|(field, _)| *field == "state")
                .map(|(_, value)| value.clone())
                .ok_or_else(|| wrong("a Hasher with a `state` field")),
            other => Err(EvalError { message: format!("hasher_finish needs a Hasher, got {}", other) }),
        },
        // ponytail: a FIXED seed, where roc randomises per process. The seed exists to
        // make hash flooding impractical, which no test here can observe, and a
        // constant keeps a Dict's iteration order reproducible between runs.
        "dict_pseudo_seed" => Ok(Value::Int(0x243F_6A88_85A3_08D3u64 as i128)),
        _ => Err(EvalError { message: format!("low-level op `{}` is not implemented", name) }),
    }
}

/// `args` is the caller's REGISTER WINDOW, not a `Vec`.
///
/// It used to be a `Vec` collected out of the registers, which cost a `malloc` per
/// builtin call — 12,006 of `list_pass`'s 34,217 allocations and 8,003 of `strings`'s.
/// Nothing here ever needed to own the vector: the dispatch layer reads `&args`, and
/// the handful of builtins that take a value out already do it with `mem::replace`,
/// which is exactly what collecting the window did anyway. So the window is passed
/// straight through, and those registers are dead after the call either way.
pub fn call_builtin_values(
    module: &str,
    name: &str,
    args: &mut [Value],
) -> Result<Value, EvalError> {
    // The low-level ops are spelled as bare names in Builtin.roc, so the compiler sends
    // them here under a module of its own rather than one of Roc's.
    if module == "LowLevel" {
        return call_low_level(name, args);
    }
    if let Some(result) = call_crypto(module, name, &args) {
        return result;
    }
    if let Some(result) = call_simd(module, name, &args) {
        return result;
    }
    if module == "Numeral" {
        return numeral::call_numeral(name, &args);
    }
    if module == "Lit" && name == "coerce" {
        return numeral::coerce(args);
    }
    // `U32.from_numeral(n)` is the width's `from_str` of the literal's text, with roc's
    // name for the failure.
    if name == "from_numeral" && is_numeric_module(module) {
        let text = args.first().and_then(numeral::numeral_text).ok_or_else(|| EvalError {
            message: format!("{}.from_numeral needs a Numeral", module),
        })?;
        return Ok(match call_builtin_values(module, "from_str", &mut [str_value(text.clone())])? {
            Value::Tag("Ok", payload) => Value::Tag("Ok", payload),
            _ => Value::tag(
                "Err",
                [Value::tag("InvalidNumeral", [str_value(format!("{} is not a {}", text, module))])],
            ),
        });
    }
    // `to_str` is dispatched on the numeric type, so it is spelled `I64.to_str`,
    // `F64.to_str`, `U8.to_str`, ... — not only `Num.to_str`. Every width shares one
    // implementation: the VALUE already carries its kind (`Int`, `U128`, `Float`,
    // `F32`, `Dec`), so the module name only has to reach it.
    // The `Encoding` protocol's own operations, which a type's `encoder_for` calls to
    // add to the text: `Encoding.encode_u32(n, state)`.
    if module == "Encoding" {
        if let Some(name) = name.strip_prefix("encode_") {
            let (written, state) = (args.first(), args.get(1));
            if let (Some(written), Some(Value::Str(state))) = (written, state) {
                let rendered = match (name, written) {
                    ("str", Value::Str(text)) => crate::eval::value::quoted(text),
                    // The method NAME says which type is being written, and an
                    // integer one writes an integer: the `1` in
                    // `Encoding.encode_u32(1, state)` is an unconstrained numeral to
                    // this interpreter and would otherwise render as `1.0`.
                    (name, _) if name.starts_with('u') || name.starts_with('i') => {
                        match as_dec(written) {
                            Some(scaled) => (scaled / DEC_SCALE).to_string(),
                            None => json_write(written),
                        }
                    }
                    _ => json_write(written),
                };
                // `encode_… : value, state -> Try(state, [])`.
                return Ok(Value::tag(
                    "Ok",
                    vec![str_value(format!("{}{}", state, rendered))],
                ));
            }
        }
    }
    if module == "Json" {
        if let Some(result) = call_json(name, &args) {
            return result;
        }
    }
    if let Some(result) = call_hasher(module, name, &args) {
        return result;
    }
    if let Some(result) = call_checked(module, name, &args) {
        return result;
    }

    // `U128` above `i128::MAX` needs genuine unsigned 128-bit arithmetic, which its
    // own handler does; everything narrower stays on the i128-based path below.
    if module == "U128" {
        if let Some(result) = call_u128(name, &args) {
            return result;
        }
    }
    // The width-aware numeric operations, before the shared `Num` handling: they are
    // the ones whose ANSWER depends on which width the module names.
    if let Some(result) = call_numeric(module, name, &args) {
        return result;
    }
    if let Some(result) = call_float(module, name, &args) {
        return result;
    }

    if name == "to_str" && is_numeric_module(module) {
        if args.len() != 1 {
            return Err(EvalError {
                message: format!("{}.to_str expects 1 argument, got {}", module, args.len()),
            });
        }
        let val = args[0].clone();
        return Ok(str_value(val.to_string()));
    }

    // `from_str` is dispatched on the numeric type too: `I64.from_str`, and so on.
    // It returns a Try, which is the tag union [Ok(a), Err(b)] — so the result is
    // an ordinary tag value. roc names the failure `BadNumStr`.
    if name == "from_str" && is_numeric_module(module) {
        if args.len() != 1 {
            return Err(EvalError {
                message: format!("{}.from_str expects 1 argument, got {}", module, args.len()),
            });
        }
        let text = match args[0].clone() {
            Value::Str(s) => s,
            other => {
                return Err(EvalError {
                    message: format!("{}.from_str needs a Str, got {}", module, other),
                })
            }
        };
        // The integer widths answered above, in `call_numeric`; these are the
        // fractional ones. `Dec` is parsed EXACTLY, digit by digit, never through
        // a float.
        let text = text.trim();
        let parsed = match module {
            "Dec" => dec_from_str(text).map(Value::Dec),
            _ => text.parse::<f64>().ok().map(Value::Float),
        };
        return Ok(match parsed {
            Some(value) => Value::tag("Ok", [value]),
            None => Value::tag("Err", [Value::bare("BadNumStr")]),
        });
    }

    // Width conversions: `I64.to_f64(n)`, `n.to_dec()`, `x.to_i64()`. Integer
    // widths are not modelled separately here, so every integer target is the same
    // conversion — only int/float actually changes the representation.
    if is_numeric_module(module) {
        if let Some(target) = name.strip_prefix("to_") {
            let value = args.first().cloned().unwrap_or(Value::Unit);
            let as_float = matches!(target, "f32" | "f64" | "dec" | "frac");
            let known = as_float
                || matches!(
                    target,
                    "u8" | "u16" | "u32" | "u64" | "u128"
                        | "i8" | "i16" | "i32" | "i64" | "i128"
                );
            if known {
                return match (value, as_float) {
                    (Value::Int(n), true) if target == "f32" => Ok(Value::F32(n as f32)),
                    (Value::Int(n), true) => Ok(Value::Float(n as f64)),
                    (Value::Int(n), false) => Ok(Value::Int(n)),
                    (Value::Float(f), true) => Ok(Value::Float(f)),
                    (Value::Float(f), false) => Ok(Value::Int(f as i128)),
                    (Value::F32(f), true) => Ok(Value::Float(f64::from(f))),
                    (Value::F32(f), false) => Ok(Value::Int(f as i128)),
                    (other, _) => Err(EvalError {
                        message: format!("Cannot convert {} to {}", other, target),
                    }),
                };
            }
        }
    }

    // Checked arithmetic. This interpreter uses f64/i64 throughout and does not
    // model saturation, so `*_try` always succeeds and `*_saturated` is the plain
    // operation. SafeMath's own Overflow branch is therefore never taken — which
    // matches roc for the inputs it uses.
    // ponytail: real saturation needs per-width arithmetic in the evaluator.
    if is_numeric_module(module) {
        let checked = name
            .strip_suffix("_try")
            .map(|op| (op, true))
            .or_else(|| name.strip_suffix("_saturated").map(|op| (op, false)));
        if let Some((op, wraps)) = checked {
            let numeric = as_f64;
            if let (Some(a), Some(b)) =
                (args.first().and_then(numeric), args.get(1).and_then(numeric))
            {
                let ints = matches!(
                    (args.first(), args.get(1)),
                    (Some(Value::Int(_)), Some(Value::Int(_)))
                );
                let result = match op {
                    // The subject comes first, so `a.minus_try(b)` is `a - b`.
                    "plus" => Some(a + b),
                    "minus" => Some(a - b),
                    "times" => Some(a * b),
                    "div" => (b != 0.0).then(|| a / b),
                    _ => None,
                };
                if let Some(value) = result {
                    let value = if ints && op != "div" {
                        Value::Int(value as i128)
                    } else {
                        Value::Float(value)
                    };
                    return Ok(if wraps { Value::tag("Ok", [value]) } else { value });
                }
            }
        }
    }

    // `negate` is what unary minus lowers to, dispatched on the numeric type.
    if name == "negate" && is_numeric_module(module) {
        if args.len() != 1 {
            return Err(EvalError {
                message: format!("{}.negate expects 1 argument, got {}", module, args.len()),
            });
        }
        return match &args[0] {
            Value::Int(n) => Ok(Value::Int(-n)),
            Value::Float(f) => Ok(Value::Float(-f)),
            Value::F32(f) => Ok(Value::F32(-f)),
            other => Err(EvalError {
                message: format!("Cannot negate {}", other),
            }),
        };
    }

    // `Range.custom`/`.iter` over a THIRD-PARTY numeric type — a `Range(Distance)`, not
    // rocflight's own integer `Value::Range`. These are record-backed and dispatch the
    // element's own `range_iter`, so they cannot go through the `List` machinery the
    // integer range shares. A record receiver (the six Range fields) is what tells the
    // two apart; an integer range is a `Value::Range` and skips this.
    if module == "Range" {
        if let Some(result) = call_range(name, &args) {
            return result;
        }
    }

    // An `Iter` is the list (or range) it walks, and `Range.size_hint` is its length.
    if matches!(module, "List" | "Iter" | "Range") {
        // A lazy iterator — or a method that must stay lazy (an unbounded source, a
        // filter that emits `Skip`, an unknown length) — is driven by `lazy`, not
        // materialized. A plain `List.map`/`fold` stays eager below.
        let on_iter = matches!(args.first(), Some(Value::Iter(_)));
        let on_lazy_source = on_iter || matches!(args.first(), Some(Value::Range { .. }));
        let lazy_method =
            // Anything on a lazy iterator is lazy.
            on_iter
            // `with_index` is `Iter`-only in roc — `Builtin.roc` declares it at
            // `Iter(a) -> Iter((U64, a))` and nowhere else — so it always produces a
            // lazy iterator, even off a list.
            //
            // `keep_if`/`drop_if` are here for a worse reason: `Builtin.roc` declares
            // BOTH `List.keep_if -> List(a)` and `Iter.keep_if -> Iter(a)`, and roc
            // inspects `[1, 2, 3].keep_if(p)` as `[2, 3]` but
            // `[1, 2, 3].iter().keep_if(p)` as `<opaque>`. Nothing here can tell those
            // apart, because `.iter()` on a list IS the list at run time — so this path
            // answers the lazy one for both. A call WRITTEN `List.keep_if(xs, p)` (or
            // piped into it) cannot be `Iter.keep_if`, and the compiler answers the eager
            // one for it (`Compiler::list_loop`). What is left divergent is method syntax
            // on a list, `[1, 2, 3].keep_if(p)`, and `List.keep_if` passed as a function
            // value: roc gives a list there and this gives an iterator. Fixing those needs
            // an `Iter` that is its own value.
            || matches!(name, "keep_if" | "drop_if" | "with_index")
            // `concat` and `size_hint` are shared with `List`: lazy only for a range
            // or an iterator, so `List.concat` of two lists stays an eager list.
            || (matches!(name, "concat" | "size_hint") && on_lazy_source)
            || (name == "from_iter" && on_iter);
        if lazy_method {
            if let Some(result) = lazy::call(name, args) {
                return result;
            }
        }
        return call_list_builtin(name, args);
    }
    if module == "Str" {
        if let Some(result) = call_str_more(name, &args) {
            return result;
        }
    }
    // `Try.map_ok(t, f)` is `t.map_ok(f)` with the receiver written first.
    if module == "Try" {
        if let Some(Value::Tag(tag @ ("Ok" | "Err"), payload)) = args.first() {
            let (tag, payload) = (*tag, payload.clone());
            if let Some(result) = try_method(tag, &payload, name, args[1..].to_vec())? {
                return Ok(result);
            }
        }
    }
    // A boxed value is the value: nothing here needs the indirection.
    if module == "Box" && matches!(name, "box" | "unbox") {
        return match args.first_mut() {
            Some(v) => Ok(std::mem::replace(v, Value::Unit)),
            None => Err(EvalError {
                message: format!("Box.{} expects 1 argument, got 0", name),
            }),
        };
    }

    match (module, name) {
        ("Bool", "not") => {
            if args.len() != 1 {
                return Err(EvalError {
                    message: format!("Bool.not expects 1 argument, got {}", args.len()),
                });
            }
            match args[0].clone() {
                Value::Bool(b) => Ok(Value::Bool(!b)),
                other => Err(EvalError {
                    message: format!("Bool.not needs a Bool, got {}", other),
                }),
            }
        }
        ("Str", "is_empty") => {
            if args.len() != 1 {
                return Err(EvalError {
                    message: format!("Str.is_empty expects 1 argument, got {}", args.len()),
                });
            }
            match &args[0] {
                Value::Str(text) => Ok(Value::Bool(text.is_empty())),
                other => Err(EvalError {
                    message: format!("Str.is_empty needs a Str, got {}", other),
                }),
            }
        }
        ("Str", "inspect") => {
            if args.len() != 1 {
                return Err(EvalError {
                    message: format!("Str.inspect expects 1 argument, got {}", args.len()),
                });
            }
            let val = args[0].clone();
            // A nominal may define `to_inspect` to control how it is shown.
            if let Some(custom) = custom_inspect(&val) {
                return Ok(custom);
            }
            Ok(str_value(inspect(&val)))
        }
        // `haystack.split_first(needle)` → `Ok({ before, after })`, or
        // `Err(NotFound)`. Splits at the FIRST occurrence only.
        ("Str", "split_first") => {
            let (text, needle) = match (args.first(), args.get(1)) {
                (Some(Value::Str(t)), Some(Value::Str(n))) => (&**t, &**n),
                _ => {
                    return Err(EvalError {
                        message: "Str.split_first needs two Str arguments".to_string(),
                    })
                }
            };
            Ok(match text.find(needle) {
                Some(i) => Value::tag(
                    "Ok",
                    vec![Value::record(vec![
                        ("before", str_value(text[..i].to_string())),
                        (
                            "after",
                            str_value(text[i + needle.len()..].to_string()),
                        ),
                    ])],
                ),
                None => Value::tag("Err", [Value::bare("NotFound")]),
            })
        }
        // `Str.join_with(list, separator)`, the subject first as always.
        ("Str", "join_with") => {
            let (items, sep) = match (args.first(), args.get(1)) {
                (Some(Value::List(items)), Some(Value::Str(sep))) => (items, &**sep),
                _ => {
                    return Err(EvalError {
                        message: "Str.join_with needs a List and a Str".to_string(),
                    })
                }
            };
            let rendered: Vec<String> = items
                .iter()
                .map(|v| match v {
                    Value::Str(text) => text.to_string(),
                    other => other.to_string(),
                })
                .collect();
            Ok(str_value(rendered.join(sep)))
        }
        ("Str", "trim") | ("Str", "trim_start") | ("Str", "trim_end")
        | ("Str", "with_ascii_uppercased") | ("Str", "with_ascii_lowercased") => {
            let text = match args.first() {
                Some(Value::Str(t)) => &**t,
                _ => {
                    return Err(EvalError {
                        message: format!("Str.{} needs a Str", name),
                    })
                }
            };
            let out = match name {
                "trim" => text.trim().to_string(),
                "trim_start" => text.trim_start().to_string(),
                "trim_end" => text.trim_end().to_string(),
                "with_ascii_uppercased" => text.to_ascii_uppercase(),
                _ => text.to_ascii_lowercase(),
            };
            Ok(str_value(out))
        }
        ("Str", "starts_with") | ("Str", "ends_with") | ("Str", "contains") => {
            let (text, needle) = match (args.first(), args.get(1)) {
                (Some(Value::Str(t)), Some(Value::Str(n))) => (&**t, &**n),
                _ => {
                    return Err(EvalError {
                        message: format!("Str.{} needs two Str arguments", name),
                    })
                }
            };
            Ok(Value::Bool(match name {
                "starts_with" => text.starts_with(needle),
                "ends_with" => text.ends_with(needle),
                _ => text.contains(needle),
            }))
        }
        ("Str", "len") | ("Str", "count_utf8_bytes") => {
            let text = match args.first() {
                Some(Value::Str(t)) => &**t,
                _ => {
                    return Err(EvalError {
                        message: format!("Str.{} needs a Str", name),
                    })
                }
            };
            Ok(Value::Int(text.len() as i128))
        }
        ("Str", "split_on") => {
            let (text, sep) = match (args.first(), args.get(1)) {
                (Some(Value::Str(t)), Some(Value::Str(sep))) => (&**t, &**sep),
                _ => {
                    return Err(EvalError {
                        message: "Str.split_on needs two Str arguments".to_string(),
                    })
                }
            };
            Ok(Value::list(
                text.split(sep)
                    .map(|part| str_value(part.to_string()))
                    .collect(),
            ))
        }
        ("Str", "from_utf8") => {
            let bytes = match args.first() {
                Some(Value::List(items)) => items,
                _ => {
                    return Err(EvalError {
                        message: "Str.from_utf8 needs a List of bytes".to_string(),
                    })
                }
            };
            let raw: Vec<u8> = bytes
                .iter()
                .filter_map(|v| match v {
                    Value::Int(n) if (0..=255).contains(n) => Some(*n as u8),
                    _ => None,
                })
                .collect();
            Ok(match String::from_utf8(raw) {
                Ok(text) => Value::tag("Ok", [str_value(text)]),
                Err(_) => Value::tag("Err", [Value::bare("BadUtf8")]),
            })
        }
        ("Str", "to_utf8") => {
            let text = match args.first() {
                Some(Value::Str(t)) => &**t,
                _ => {
                    return Err(EvalError {
                        message: "Str.to_utf8 needs a Str".to_string(),
                    })
                }
            };
            Ok(Value::list(
                text.as_bytes().iter().map(|b| Value::Int(*b as i128)).collect(),
            ))
        }
        ("Str", "concat") => {
            let mut result = String::new();
            for val in args.iter() {
                match val {
                    Value::Str(text) => result.push_str(text),
                    other => result.push_str(&other.to_string()),
                }
            }
            Ok(str_value(result))
        }
        // The `Encoding` protocol's DERIVED halves. `Builtin.roc` declares
        // `parser_for`/`encoder_for` on every type and the compiler derives most of
        // them from the shape; a name with no definition here — including `Elem` from
        // a `parser_for` that delegates through its type parameter — gets the derived
        // one, which reads or writes whatever the shape on the descriptor stack says.
        (_, "parser_for") if args.len() == 1 => Ok(Value::Builtin("Json.elem_parse", 1)),
        (_, "encoder_for") if args.len() == 1 => Ok(Value::Builtin("Json.elem_encode", 2)),
        _ => {
            // A platform's hosted effect: the host's own code, reached through the
            // registry when rocflight is linked into that host.
            if let Some(answer) = crate::platform::hosted::call(module, name, &args) {
                return answer.map_err(|message| EvalError { message });
            }
            Err(EvalError {
                // A platform that declares this really does provide it: the gap is
                // that its signature could not be laid out for the host (a type the
                // loaded modules do not declare), and `--show-platforms` names which.
                message: if crate::platform::real::is_declared(module, name) {
                    format!(
                        "`{}.{}` is an effect the platform declares, but its signature could \
                         not be laid out for the host; run with --show-platforms to see why",
                        module, name
                    )
                } else {
                    format!("Unknown function {}.{}", module, name)
                },
            })
        }
    }
}

/// Apply binary operation
/// Route an operator to a user-defined method, if the operand type has one.
///
/// roc spells every operator as a method — `a + b` IS `a.plus(b)` — so a nominal
/// that defines `plus` gets `+`. Only tried for compound values: a type defining
/// `plus` must not hijack `1 + 2`, and the builtin path handles the primitives.
///
/// Dispatch is by method NAME, not by type, because values carry no nominal tag at
/// runtime — the same search `Dispatch` already does.
pub fn dispatch_operator(
    op: BinOp,
    left: &Value,
    right: &Value,
) -> Result<Option<Value>, EvalError> {
    if !matches!(left, Value::Record(_) | Value::Tag(..) | Value::Tuple(_)) {
        return Ok(None);
    }
    let method = match op {
        BinOp::Add => "plus",
        BinOp::Sub => "minus",
        BinOp::Mul => "times",
        BinOp::Div => "div_by",
        BinOp::IntDiv => "div_trunc_by",
        BinOp::Rem => "rem_by",
        BinOp::Lt => "is_lt",
        BinOp::Gt => "is_gt",
        BinOp::Le => "is_lte",
        BinOp::Ge => "is_gte",
        // `!=` is `is_eq` negated: roc asks for `is_eq` either way.
        BinOp::Eq | BinOp::Ne => "is_eq",
        _ => return Ok(None),
    };

    let Some((_, func)) = crate::vm::best_method(method, left) else {
        return Ok(None);
    };
    let result = call_function(func, vec![left.clone(), right.clone()])?;
    Ok(Some(match (op, result) {
        (BinOp::Ne, Value::Bool(b)) => Value::Bool(!b),
        (_, other) => other,
    }))
}

/// Apply a binary operator to two already-evaluated operands.
///
/// An associated function rather than a method: it needs no evaluator state, and
/// the VM backend calls it too. Operator SEMANTICS live in exactly one place, so
/// the two engines cannot drift on what `//` does to a negative number.
///
/// The operands are BORROWED. No arm here needs to own them — every one either
/// copies a scalar out or builds a new value — and cloning them cost the VM 40% of
/// `fib`: two 32-byte `Value` clones per arithmetic instruction, for two integers.
/// The alternative was a fast path for `Int` inside the VM's `Bin` opcode, which
/// would have been a second implementation of Roc's arithmetic; this is the same
/// win with one.
pub fn apply_binop(op: BinOp, left: &Value, right: &Value) -> Result<Value, EvalError> {
    // Two integers, which is most arithmetic in most programs, and the case the VM's
    // `BinInt` opcode skips this dispatch for entirely.
    if let (Value::Int(a), Value::Int(b)) = (left, right) {
        if let Some(result) = int_binop(op, *a, *b) {
            return result;
        }
    }
    // A `U128` on either side keeps the whole operation unsigned 128-bit — an `Int`
    // beside one is a small `U128` reinterpreted. The plain arithmetic operators
    // crash on overflow, as roc's do.
    if matches!(left, Value::U128(_)) || matches!(right, Value::U128(_)) {
        if let (Some(a), Some(b)) = (as_u128_bits(left), as_u128_bits(right)) {
            let crash = |what: &str| Err(EvalError { message: format!("crash: U128 {}", what) });
            let checked = |o: Option<u128>| match o {
                Some(n) => Ok(Value::U128(n)),
                None => crash("overflowed"),
            };
            return match op {
                BinOp::Add => checked(a.checked_add(b)),
                BinOp::Sub => checked(a.checked_sub(b)),
                BinOp::Mul => checked(a.checked_mul(b)),
                BinOp::IntDiv if b == 0 => crash("divided by zero"),
                BinOp::IntDiv => Ok(Value::U128(a / b)),
                BinOp::Rem if b == 0 => crash("divided by zero"),
                BinOp::Rem => Ok(Value::U128(a % b)),
                BinOp::Eq => Ok(Value::Bool(a == b)),
                BinOp::Ne => Ok(Value::Bool(a != b)),
                BinOp::Lt => Ok(Value::Bool(a < b)),
                BinOp::Le => Ok(Value::Bool(a <= b)),
                BinOp::Gt => Ok(Value::Bool(a > b)),
                BinOp::Ge => Ok(Value::Bool(a >= b)),
                _ => crash("unsupported operator"),
            };
        }
    }
    // A `Dec` on either side makes the whole operation fixed point. An ordinary
    // integer beside one is widened, which is how `total / n` works when `n` is a
    // length.
    if matches!(left, Value::Dec(_)) || matches!(right, Value::Dec(_)) {
        if let (Some(a), Some(b)) = (as_dec(left), as_dec(right)) {
            if let Some(result) = dec_binop(op, a, b) {
                return result;
            }
        }
    }

    // An `F32` on either side keeps the arithmetic in f32, which is where roc rounds
    // it: `0.1.F32 + 0.2.F32` is `0.3`, not the f64 sum of two f32s.
    if matches!(left, Value::F32(_)) || matches!(right, Value::F32(_)) {
        if let (Some(a), Some(b)) = (as_f64(left), as_f64(right)) {
            let (a, b) = (a as f32, b as f32);
            let result = match op {
                BinOp::Add => Some(a + b),
                BinOp::Sub => Some(a - b),
                BinOp::Mul => Some(a * b),
                BinOp::Div => Some(a / b),
                _ => None,
            };
            if let Some(result) = result {
                return Ok(Value::F32(result));
            }
        }
    }

    match (op, left, right) {
        // Arithmetic on integers
        (BinOp::Add, Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
        (BinOp::Sub, Value::Int(a), Value::Int(b)) => Ok(Value::Int(a - b)),
        (BinOp::Mul, Value::Int(a), Value::Int(b)) => Ok(Value::Int(a * b)),
        (BinOp::Div, Value::Int(a), Value::Int(b)) => {
            if *b == 0 {
                Err(EvalError { message: "Division by zero".to_string() })
            } else {
                Ok(Value::Int(a / b))
            }
        }
        // `//` truncating division and `%` remainder — integers only in Roc.
        (BinOp::IntDiv, Value::Int(a), Value::Int(b)) => {
            if *b == 0 {
                Err(EvalError { message: "Division by zero".to_string() })
            } else {
                // Roc's `//` truncates toward zero, which is Rust's `/` for i64.
                Ok(Value::Int(a / b))
            }
        }
        (BinOp::Rem, Value::Int(a), Value::Int(b)) => {
            if *b == 0 {
                Err(EvalError { message: "Division by zero".to_string() })
            } else {
                Ok(Value::Int(a % b))
            }
        }
        // Arithmetic on floats
        (BinOp::Add, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
        (BinOp::Sub, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a - b)),
        (BinOp::Mul, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a * b)),
        // IEEE division: `1.0 / 0.0` is infinity in roc, not a crash.
        (BinOp::Div, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a / b)),
        // Mixed int/float arithmetic
        (BinOp::Add, Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 + b)),
        (BinOp::Add, Value::Float(a), Value::Int(b)) => Ok(Value::Float(a + *b as f64)),
        (BinOp::Sub, Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 - b)),
        (BinOp::Sub, Value::Float(a), Value::Int(b)) => Ok(Value::Float(a - *b as f64)),
        (BinOp::Mul, Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 * b)),
        (BinOp::Mul, Value::Float(a), Value::Int(b)) => Ok(Value::Float(a * *b as f64)),
        (BinOp::Div, Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 / b)),
        (BinOp::Div, Value::Float(a), Value::Int(b)) => Ok(Value::Float(a / *b as f64)),
        // String concatenation
        (BinOp::Add, Value::Str(a), Value::Str(b)) => {
            let concatenated = format!("{}{}", a, b);
            Ok(str_value(concatenated))
        }
        // Comparison operators — these yield Bool in Roc, not 0/1.
        (BinOp::Eq, a, b) => Ok(Value::Bool(values_equal(a, b))),
        (BinOp::Ne, a, b) => Ok(Value::Bool(!values_equal(a, b))),
        (BinOp::Lt, a, b) => compare(a, b, |o| o == std::cmp::Ordering::Less),
        (BinOp::Le, a, b) => compare(a, b, |o| o != std::cmp::Ordering::Greater),
        (BinOp::Gt, a, b) => compare(a, b, |o| o == std::cmp::Ordering::Greater),
        (BinOp::Ge, a, b) => compare(a, b, |o| o != std::cmp::Ordering::Less),
        // `and` / `or` are Bool-only in Roc. Both operands are already
        // evaluated, so these do not short-circuit — see the ponytail note.
        (BinOp::And, Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(*a && *b)),
        (BinOp::Or, Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(*a || *b)),
        // Numeric operands to `and`/`or`, kept for the `1 && 0` spelling used
        // before Bool existed. ponytail: drop once nothing relies on it.
        (BinOp::And, a, b) => Ok(Value::Bool(is_truthy(a) && is_truthy(b))),
        (BinOp::Or, a, b) => Ok(Value::Bool(is_truthy(a) || is_truthy(b))),
        (op, left, right) => {
            Err(EvalError {
                // Naming them: "Invalid operands for operator" left no way to tell
                // which operator, on what, without a bisect.
                message: format!("Invalid operands for {:?}: {} and {}", op, left, right),
            })
        }
    }
}

/// Compare two numbers and turn the ordering into a Bool.
///
/// Replaces four near-identical arms per operator (int/int, float/float and both
/// mixed pairings); only the predicate differs between `<`, `<=`, `>` and `>=`.
fn compare(
    a: &Value,
    b: &Value,
    keep: impl Fn(std::cmp::Ordering) -> bool,
) -> Result<Value, EvalError> {
    let ordering = match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        _ if as_f64(a).is_some() && as_f64(b).is_some() => {
            cmp_f64(as_f64(a).expect("checked"), as_f64(b).expect("checked"))?
        }
        _ => {
            return Err(EvalError {
                message: "Comparison operators need numbers".to_string(),
            })
        }
    };
    Ok(Value::Bool(keep(ordering)))
}

/// Total-order two f64s, rejecting NaN rather than silently reporting `false`.
fn cmp_f64(x: f64, y: f64) -> Result<std::cmp::Ordering, EvalError> {
    x.partial_cmp(&y).ok_or_else(|| EvalError {
        message: "Cannot compare NaN".to_string(),
    })
}

/// Check if two values are equal
pub fn values_equal(a: &Value, b: &Value) -> bool {
    // A component that is a nominal with its own `is_eq` — a `Dict`, a `Set` — is
    // compared by that method, not by its erased layout: two dicts with the same
    // entries in a different insertion order are equal, and roc honours that even when
    // the dict is nested inside a record, tuple, tag or list. The method (e.g.
    // `Dict.is_eq`) compares entries, never the whole container, so this terminates.
    if matches!(a, Value::Tag(..) | Value::Record(_)) {
        if let Some((_, func)) = crate::vm::best_method("is_eq", a) {
            if let Ok(Value::Bool(equal)) = call_function(func, vec![a.clone(), b.clone()]) {
                return equal;
            }
        }
    }
    match (a, b) {
        (Value::Str(s1), Value::Str(s2)) => s1 == s2,
        (Value::Int(n1), Value::Int(n2)) => n1 == n2,
        // A `U128`, and a small one held as an `Int` beside it, compare by bit pattern.
        (Value::U128(x), Value::U128(y)) => x == y,
        (Value::U128(x), Value::Int(y)) | (Value::Int(y), Value::U128(x)) => *x == *y as u128,
        (Value::Dec(d1), Value::Dec(d2)) => d1 == d2,
        // `expect safe_variance([0]) == Ok(0)` compares a Dec against a whole number.
        (Value::Dec(d), Value::Int(n)) | (Value::Int(n), Value::Dec(d)) => {
            as_dec(&Value::Int(*n)).is_some_and(|scaled| scaled == *d)
        }
        (Value::Bool(b1), Value::Bool(b2)) => b1 == b2,
        (Value::Unit, Value::Unit) => true,
        (Value::Missing, Value::Missing) => true,
        (Value::Float(f1), Value::Float(f2)) => (f1 - f2).abs() < 1e-10,
        (Value::Int(n), Value::Float(f)) => ((*n as f64) - f).abs() < 1e-10,
        (Value::Float(f), Value::Int(n)) => (f - (*n as f64)).abs() < 1e-10,
        // An f32 is exact about itself; against anything else it is its f64 value.
        (Value::F32(f1), Value::F32(f2)) => f1 == f2,
        (Value::F32(f), other) | (other, Value::F32(f)) => {
            as_f64(other).is_some_and(|x| (x - f64::from(*f)).abs() < 1e-6)
        }
        // Bare tags compare by name; payload tags compare payloads too.
        (Value::Tag(n1, p1), Value::Tag(n2, p2)) => {
            n1 == n2
                && p1.len() == p2.len()
                && p1.iter().zip(p2.iter()).all(|(a, b)| values_equal(a, b))
        }
        // Tuples are equal element-wise at the same arity.
        (Value::Tuple(x), Value::Tuple(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| values_equal(a, b))
        }
        // Lists are equal element-wise, and only at the same length.
        (Value::List(x), Value::List(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| values_equal(a, b))
        }
        // Field order is not part of a record's identity: `{ a: 1, b: 2 }` is
        // `{ b: 2, a: 1 }`.
        (Value::Record(x), Value::Record(y)) => {
            x.len() == y.len()
                && x.iter().all(|(name, v1)| {
                    y.iter().any(|(other, v2)| name == other && values_equal(v1, v2))
                })
        }
        _ => false,
    }
}

/// Check if a value is truthy (non-zero for numbers, non-empty for strings)
fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Int(n) => *n != 0,
        Value::Float(f) => *f != 0.0,
        Value::F32(f) => *f != 0.0,
        Value::Str(s) => !s.is_empty(),
        _ => true, // lambdas and builtins are truthy
    }
}


/// Is `module` one of Roc's numeric types (or the generic `Num`)?
pub fn is_numeric_module(module: &str) -> bool {
    matches!(
        module,
        "Num"
            | "U8" | "U16" | "U32" | "U64" | "U128"
            | "I8" | "I16" | "I32" | "I64" | "I128"
            | "F32" | "F64"
            | "Dec"
    )
}

/// Render a value the way Roc's `Str.inspect` does.
///
/// Verified against `roc` nightly-2026-09-03:
///   * record fields are sorted **alphabetically**, not left in source order —
///     `{ zebra: 1, apple: 2 }` inspects as `{ apple: ..., zebra: ... }`
///   * sorting is recursive, so nested records are sorted too
///   * strings are quoted: `"hi"`
///   * booleans are `True` / `False`
///   * the empty record is `{}`
///
/// ponytail: integers render without a fractional part (`42`). Roc prints `42.0`
/// for an integer literal that nothing constrains, because it defaults to a
/// fractional type — but `42` once annotated `I64`. Matching that needs the type
/// checker to track numeric types (numeric-types phase); until then, annotate
/// numbers in any test that inspects them, which the golden pairs already do.
pub fn inspect(value: &Value) -> String {
    // A nominal's own `to_inspect` controls how it shows, at the top level and nested
    // alike — roc applies it wherever the value appears. The `INSPECTING` guard inside
    // keeps a `to_inspect` that calls `Str.inspect(self)` from looping.
    if let Some(Value::Str(shown)) = custom_inspect(value) {
        return shown.to_string();
    }
    // An OPAQUE nominal shows as `<opaque>`: roc will not print the inside of one, and
    // `AllSyntax`'s `Secret :: { key : Str }` relies on that to keep its key hidden.
    if matches!(value, Value::Record(_) | Value::Tag(..) | Value::Tuple(_))
        && crate::vm::is_opaque(value)
    {
        return "<opaque>".to_string();
    }
    match value {
        Value::Str(s) => crate::eval::value::quoted(s),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Unit => "{}".to_string(),
        Value::List(items) => {
            let rendered: Vec<String> = items.iter().map(inspect).collect();
            format!("[{}]", rendered.join(", "))
        }
        Value::Tuple(items) => {
            let rendered: Vec<String> = items.iter().map(inspect).collect();
            format!("({})", rendered.join(", "))
        }
        Value::Range { .. } => "<opaque>".to_string(),
        Value::Record(fields) => {
            if fields.is_empty() {
                return "{}".to_string();
            }
            let mut sorted: Vec<&(&str, Value)> = fields.iter().collect();
            sorted.sort_by(|a, b| a.0.cmp(b.0));
            let rendered: Vec<String> = sorted
                .iter()
                .map(|(name, v)| format!("{}: {}", name, inspect(v)))
                .collect();
            format!("{{ {} }}", rendered.join(", "))
        }
        Value::Tag(name, args) if args.is_empty() => name.to_string(),
        Value::Tag(name, args) => {
            let rendered: Vec<String> = args.iter().map(inspect).collect();
            format!("{}({})", name, rendered.join(", "))
        }
        // roc shows every function the same way, whatever its parameters.
        Value::Closure(_) | Value::Builtin(..) => "<function>".to_string(),
        other => other.to_string(),
    }
}

/// Does `pattern` match `value`? Collects any bindings it introduces.
///
/// Bindings are pushed onto `bindings` rather than bound directly so a partial match
/// leaves no trace: a nested pattern can bind several names and then fail on the last
/// element, and those names must not leak into the next arm.
/// Run one `expect` inside a function body: a runtime assertion, not a test.
///
/// A failure is reported and execution CONTINUES — roc prints to stderr and carries on
/// rather than aborting — but the run ends up exiting non-zero. It is never tallied:
/// `roc test` counts top-level `expect`s only, even when one of them calls a function
/// whose own assertion fires. Shared with the VM's `Expect` opcode.
pub fn run_expect(value: &Value) -> Result<(), EvalError> {
    match value {
        Value::Bool(true) => Ok(()),
        Value::Bool(false) => {
            ASSERT_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
            report(Report::ExpectFailed, "expect failed");
            Ok(())
        }
        other => Err(EvalError {
            message: format!("`expect` needs a Bool, got {}", other),
        }),
    }
}

/// Run one TOP-LEVEL `expect`: a test, tallied for the `roc test` report.
pub fn run_test_expect(value: &Value) -> Result<(), EvalError> {
    expect_ran();
    match value {
        Value::Bool(false) => {
            expect_failed();
            report(Report::ExpectFailed, "expect failed");
            Ok(())
        }
        _ => run_expect(value),
    }
}

/// `dbg value` — to stderr, so it never mixes into a program's output.
pub fn run_dbg(value: &Value) {
    report(Report::Dbg, &inspect(value));
}

/// What a program has to say outside its output: a `dbg`, a failed inline `expect`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Report {
    Dbg,
    ExpectFailed,
}

/// Where reports go. By default to stderr, the way `roc` prints them. When rocflight
/// is linked into a platform's host the host owns the process and its `roc_dbg` /
/// `roc_expect_failed` are what should hear them, so the host installs itself here.
static REPORTER: std::sync::OnceLock<fn(Report, &str)> = std::sync::OnceLock::new();

/// Route reports to `to` for the rest of the process. Only the first caller wins.
pub fn set_reporter(to: fn(Report, &str)) {
    let _ = REPORTER.set(to);
}

fn report(kind: Report, message: &str) {
    match REPORTER.get() {
        Some(to) => to(kind, message),
        None => match kind {
            Report::Dbg => eprintln!("[dbg] {}", message),
            Report::ExpectFailed => eprintln!("Expect failed: {}", message),
        },
    }
}

/// `crash "message"`. A Str crashes with its text; anything else with its rendering.
pub fn crash_error(value: &Value) -> EvalError {
    let text = match value {
        Value::Str(s) => s.to_string(),
        other => other.to_string(),
    };
    EvalError { message: format!("crash: {}", text) }
}

/// Integer arithmetic and comparison — the one implementation.
///
/// `None` for `and`/`or`, which are Bool-only in Roc and reach integers only through a
/// spelling kept for compatibility; those stay in `apply_binop`.
///
/// Its own function so the VM's `BinInt` opcode can compute a result without going
/// through `apply_binop`'s dispatch on operand shapes, which is worth about 24% of
/// `fib` — while integer semantics still exist in exactly one place.
pub fn int_binop(op: BinOp, a: i128, b: i128) -> Option<Result<Value, EvalError>> {
    let divide_by_zero = || {
        Some(Err(EvalError { message: "Division by zero".to_string() }))
    };
    Some(Ok(match op {
        BinOp::Add => Value::Int(a + b),
        BinOp::Sub => Value::Int(a - b),
        BinOp::Mul => Value::Int(a * b),
        // Roc's `//` truncates toward zero, which is Rust's `/` for i64.
        BinOp::Div | BinOp::IntDiv => {
            if b == 0 {
                return divide_by_zero();
            }
            Value::Int(a / b)
        }
        BinOp::Rem => {
            if b == 0 {
                return divide_by_zero();
            }
            Value::Int(a % b)
        }
        BinOp::Eq => Value::Bool(a == b),
        BinOp::Ne => Value::Bool(a != b),
        BinOp::Lt => Value::Bool(a < b),
        BinOp::Le => Value::Bool(a <= b),
        BinOp::Gt => Value::Bool(a > b),
        BinOp::Ge => Value::Bool(a >= b),
        BinOp::And | BinOp::Or => return None,
    }))
}

/// How a value renders INSIDE a string interpolation.
///
/// Not the same as `Display`: a `Str` interpolates without its quotes. Shared with the
/// VM's `Interp` opcode so `"x=${s}"` cannot come out differently on the two engines.
pub fn interpolated(value: &Value) -> String {
    match value {
        Value::Str(s) => s.to_string(),
        Value::Int(n) => n.to_string(),
        // Matches `Value`'s own Display: no trailing `.0` on a whole float, `nan`.
        Value::Float(f) => Value::Float(*f).to_string(),
        Value::F32(f) => Value::F32(*f).to_string(),
        Value::Builtin(name, arity) => format!("<{}/{}>", name, arity),
        other => other.to_string(),
    }
}

/// Dispatch `method` on an evaluated receiver, the BUILTIN way.
///
/// A nominal's own method block is not consulted here: the compiler resolves that to a
/// chunk and calls it directly. Everything after that point is this function — the
/// `iter` shorthand, the `Try` methods, then the module's builtin table.
/// `values` is the caller's REGISTER WINDOW: the receiver at slot 0 and the arguments
/// after it, which is already the shape every builtin wants — `xs.map(f)` is
/// `List.map(xs, f)`. It used to arrive as a `Vec` collected out of the registers, with
/// the receiver then `remove`d off the front and a SECOND `Vec` built to put it back:
/// two allocations and an O(n) shift on every method call that reaches a builtin.
pub fn dispatch_builtin(method: &str, values: &mut [Value]) -> Result<Value, EvalError> {
    let (receiver, args) = values.split_first().ok_or_else(|| EvalError {
        message: format!("`{}` was dispatched on nothing", method),
    })?;
    let receiver = receiver.clone();
    // `.iter()` on something already iterable is the identity. A range STAYS a range:
    // building the list of its elements cost 190 MB on a two-million-element range, and
    // every builtin that only walks the elements can walk a range instead. It also
    // matches roc, which inspects a range as `<opaque>` rather than as a list.
    //
    // ponytail: still eager in the sense that `map` over a range builds its output
    // list. Fusing `map` into the consumer needs a real lazy iterator; this removes the
    // ceiling without one.
    if method == "iter" && args.is_empty() {
        if matches!(receiver, Value::List(_) | Value::Range { .. }) {
            return Ok(receiver);
        }
    }

    // The `Encoding` protocol's reading half. A type's own `parser_for` dispatches
    // these ON the encoding it was handed — `encoding.parse_u32(state)` — and the
    // encoding this interpreter hands out is the marker below.
    if let Value::Tag(name, _) = &receiver {
        if &**name == JSON_ENCODING {
            if let Some(read) = method.strip_prefix("parse_") {
                return json_parse_piece(read, args.first());
            }
        }
    }

    // `Ok`/`Err` carry no module, but they answer the Try methods. Done here rather
    // than in `module_for` because the payload has to be rebuilt around the result.
    if let Value::Tag(tag, payload) = &receiver {
        if matches!(*tag, "Ok" | "Err") {
            if let Some(result) = try_method(tag, payload, method, args.to_vec())? {
                return Ok(result);
            }
        }
    }

    // A crypto `Digest`/`Hasher` value answers its methods in method syntax too —
    // `digest.to_hex()`, `hasher.write(bytes).finish()`.
    if let Value::Tag(tag @ ("CryptoDigest" | "CryptoHasher"), payload) = &receiver {
        let algo = if *tag == "CryptoHasher" {
            match payload.first() { Some(Value::Str(a)) => a.to_string(), _ => "Sha256".to_string() }
        } else {
            "Sha256".to_string()
        };
        let module = if *tag == "CryptoHasher" { format!("{}Hasher", algo) } else { format!("{}Digest", algo) };
        if let Some(result) = call_crypto(&module, method, values) {
            return result;
        }
    }
    // `to_hash` on a record, tuple or tag — a structural `Dict`/`Set` key — hashes
    // structurally: none of the nominal `to_hash` blocks fit, so the builtin does it.
    if method == "to_hash" {
        if let Some(result) = call_hasher("Builtin", method, values) {
            return result;
        }
    }
    // A record-backed `Range(third-party)` — `range.iter()`, `.size_hint()` — carries no
    // module, so it reaches here; its six fields say it is a Range, and `call_range`
    // dispatches the element's own `range_iter`.
    if let Value::Record(fields) = &receiver {
        if fields.iter().any(|(name, _)| *name == "lower")
            && fields.iter().any(|(name, _)| *name == "len_if_known")
        {
            if let Some(result) = call_range(method, values) {
                return result;
            }
        }
    }

    // `value.encode(format)` is `format.encode_<kind>(value)` — `Builtin.roc` declares
    // `encode` on every scalar as exactly that one hop, and rocflight does not load
    // those members. The format supplies the real work.
    if method == "encode" && args.len() == 1 {
        if let Some(kind) = module_for(&receiver).map(|m| m.to_ascii_lowercase()) {
            let named = format!("encode_{}", kind);
            if let Some((_, func)) = crate::vm::best_method(&named, &args[0]) {
                return call_function(func, vec![args[0].clone(), receiver]);
            }
        }
    }

    let module = module_for(&receiver).ok_or_else(|| EvalError {
        message: format!("Cannot dispatch `{}` on {}", method, receiver),
    })?;

    // The receiver is already the FIRST argument, which is why roc's builtins take
    // their subject first: `xs.map(f)` is `List.map(xs, f)`.
    call_builtin_values(module, method, values)
}

/// Run one of the default host's effects on already-evaluated arguments.
///
/// Shared with the VM's `CallHost` opcode. The host is the only reason a pure language
/// prints anything, so both engines have to reach the same one.
pub fn host_effect(name: &str, args: Vec<Value>) -> Result<Value, EvalError> {
    let (params, _) = crate::platform::host::lookup(name).ok_or_else(|| EvalError {
        message: format!("Unknown host effect '{}'", name),
    })?;
    if args.len() != params.len() {
        return Err(EvalError {
            message: format!("{} expects {} argument(s), got {}", name, params.len(), args.len()),
        });
    }
    match name {
        "echo!" => {
            match &args[0] {
                Value::Str(s) => print!("{}", s),
                other => print!("{}", other),
            }
            use std::io::Write;
            let _ = std::io::stdout().flush();
            Ok(Value::Unit)
        }
        _ => Err(EvalError {
            message: format!("Host effect '{}' is declared but not implemented", name),
        }),
    }
}

/// The captures of a string pattern against `text`, or `None` if it does not match.
///
/// `"foo${name}bar"` against `"foo123bar"` is `["123"]`: the prefix must open the
/// string, each capture runs up to the FIRST occurrence of the literal after it, and a
/// last segment with no literal takes the rest. Nothing may be left over.
pub fn interp_captures(prefix: &str, segments: &[(&str, &str)], text: &str) -> Option<Vec<Value>> {
    let mut rest = text.strip_prefix(prefix)?;
    let mut captures = Vec::with_capacity(segments.len());
    for (i, (_, literal)) in segments.iter().enumerate() {
        if literal.is_empty() && i + 1 == segments.len() {
            captures.push(str_value(rest.to_string()));
            rest = "";
        } else {
            // A capture stops at the FIRST byte of its delimiter, as roc's does:
            // `"foo${bar}baz"` against `fooleftbzzbaz` captures `left` and then fails,
            // rather than scanning ahead for a later `baz`.
            let at = rest.find(literal.chars().next()?)?;
            let captured = rest[..at].to_string();
            rest = rest[at..].strip_prefix(literal)?;
            captures.push(str_value(captured));
        }
    }
    rest.is_empty().then_some(captures)
}

/// Does a LITERAL pattern — `1`, `3.5`, `"hello"` — match `value`?
///
/// Its own function because the VM's `TestLit` opcode calls it too, and because these
/// rules are NOT `values_equal`'s: a pattern matches a value of the same kind only, so
/// `1` does not match `1.0`, where `1 == 1.0` is `True`. Two engines disagreeing about
/// that would be a wrong answer in a `match`, not an error.
pub fn literal_pattern_matches(pattern: &Pattern, value: &Value) -> bool {
    match (pattern, value) {
        (Pattern::Int(expected), Value::Int(n)) => n == expected,
        // A numeral pattern against a `Dec` scrutinee is a `Dec` literal: `1` in
        // `match (1, 2) { (1, b) => b }` is `1.0`, as both defaulted.
        (Pattern::Int(expected), Value::Dec(d)) => expected.checked_mul(DEC_SCALE) == Some(*d),
        (Pattern::Float(_, exact), Value::Dec(d)) => exact == d,
        // The same tolerance `values_equal` uses for two floats.
        (Pattern::Float(expected, _), Value::Float(n)) => (n - expected).abs() < 1e-10,
        (Pattern::Float(expected, _), Value::F32(n)) => (f64::from(*n) - expected).abs() < 1e-6,
        (Pattern::Str(expected), Value::Str(s)) => &**s == *expected,
        (Pattern::StrInterp { prefix, segments }, Value::Str(s)) => {
            interp_captures(prefix, segments, s).is_some()
        }
        _ => false,
    }
}

/// Call a function value with already-evaluated arguments.
///
/// The builtins' callback hook: `xs.map(f)` reaches a function through here, whether
/// `f` is a closure the compiler produced or a builtin passed as a value.
pub fn call_function(func: Value, mut args: Vec<Value>) -> Result<Value, EvalError> {
    match func {
        Value::Closure(closure) => crate::vm::call_closure(&closure, args),
        // A builtin passed as a value — `xs.map(Str.inspect)`.
        Value::Builtin(qualified, _) => {
            let (module, name) = qualified.split_once('.').ok_or_else(|| EvalError {
                message: format!("`{}` is not a qualified builtin", qualified),
            })?;
            call_builtin_values(module, name, &mut args)
        }
        other => Err(EvalError {
            message: format!("Attempted to call a non-function value: {}", other),
        }),
    }
}

/// How many `expect`s ran, and how many failed.
///
/// Process-wide rather than a field, because every call builds its own `Evaluator` —
/// an `expect` inside a function would otherwise be counted by a tally that is thrown
/// away when the call returns. `roc test` reports these totals.
static EXPECTS_RUN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static EXPECTS_FAILED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn expect_ran() {
    EXPECTS_RUN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

fn expect_failed() {
    EXPECTS_FAILED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Did an `expect` inside a function body fail? A normal run exits 1 if so.
static ASSERT_FAILED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Whether any in-function `expect` failed during this run.
pub fn assert_failed() -> bool {
    ASSERT_FAILED.load(std::sync::atomic::Ordering::Relaxed)
}

/// `(ran, failed)` for every top-level `expect` evaluated so far.
pub fn expect_tally() -> (usize, usize) {
    (
        EXPECTS_RUN.load(std::sync::atomic::Ordering::Relaxed),
        EXPECTS_FAILED.load(std::sync::atomic::Ordering::Relaxed),
    )
}

/// Which builtin module a value dispatches to.
///
/// The checker resolves dispatch from the receiver's TYPE; this resolves the same
/// thing from its runtime kind, and the two agree because a value's kind follows its
/// type. Numbers report `I64`/`F64` because the interpreter keeps one representation
/// of each — `is_numeric_module` accepts every numeric name, so `small.to_str()` on a
/// U8 lands in the same place.
///
/// `None` for records and tags: a nominal's methods would be found through its
/// declared type, but values carry no nominal wrapper (roc erases it), so there is
/// nothing at runtime to dispatch on. See the phase-20 notes.
pub fn module_for(value: &Value) -> Option<&'static str> {
    Some(match value {
        Value::Cell(_) => return None,
        Value::Int(_) => "I64",
        Value::U128(_) => "U128",
        Value::Simd { kind, .. } => simd_type_name(*kind),
        Value::Float(_) => "F64",
        Value::F32(_) => "F32",
        Value::Dec(_) => "Dec",
        Value::Str(_) => "Str",
        Value::Bool(_) => "Bool",
        Value::List(_) => "List",
        // A range answers the List methods: `(1..=n).iter().fold(..)` is the idiom,
        // and the ones that only walk the elements never build a list.
        Value::Range { .. } => "List",
        // A lazy iterator answers the List and Iter methods.
        Value::Iter(_) => "List",
        Value::Record(_) | Value::Tag(..) | Value::Tuple(_) | Value::Unit | Value::Missing => return None,
        Value::Closure(..) | Value::Builtin(..) => return None,
    })
}

// --- JSON ----------------------------------------------------------------------------

/// The encoding value handed to a type's `parser_for` and `encoder_for`.
///
/// `Builtin.roc` has a real `JsonEncoding` with its own methods; this is a marker that
/// `dispatch_builtin` recognises, which is the same idea with the 108 members of the
/// protocol left out.
const JSON_ENCODING: &str = "#JsonEncoding";

/// One `encoding.parse_…(state)` step: read a piece off the front of the remaining
/// text and hand back what was read with the rest.
fn json_parse_piece(read: &str, state: Option<&Value>) -> Result<Value, EvalError> {
    let text = match state {
        Some(Value::Str(s)) => s.to_string(),
        other => {
            return Err(EvalError {
                message: format!("a parser's state is the text left to read, got {:?}", other),
            })
        }
    };
    let mut cursor = JsonCursor { bytes: text.as_bytes(), at: 0 };
    cursor.space();
    // `parse_null` is the odd one: it answers the REMAINING text, not a value with it,
    // because there is no value in a `null`.
    if read == "null" {
        return Ok(if cursor.eat("null") {
            Value::tag("Ok", [str_value(text[cursor.at..].to_string())])
        } else {
            Value::tag("Err", [Value::tag("InvalidJson", [str_value(text)])])
        });
    }
    let value = match read {
        "str" => cursor.string().map(str_value),
        _ => cursor.value(),
    };
    Ok(match value {
        Some(value) => Value::tag(
            "Ok",
            vec![Value::record(vec![
                ("rest", str_value(text[cursor.at..].to_string())),
                ("value", value),
            ])],
        ),
        None => Value::tag(
            "Err",
            vec![Value::tag("InvalidJson", [str_value(text)])],
        ),
    })
}

/// `Json.parse` and `Json.to_str`.
///
/// `Builtin.roc` derives these from the SHAPE being decoded, through the `Encoding`
/// protocol at `Builtin.roc:60` — 108 members of it. rocflight has no derivation, so it
/// reads JSON into its own values instead: an object is a record, an array a list, a
/// string a Str. Roc records are structural and rocflight erases nominals, so
/// `record.image.title` lands on a plain field and the shape never has to be consulted.
///
/// `ponytail: shape-blind. The annotation is not read, so a mismatch surfaces as a
/// missing field where it is used rather than as an `Err` at the parse, and a number
/// arrives as the interpreter's own integer or Dec rather than the annotated width.
/// Deriving from the shape is what closes that, and needs the Encoding protocol.`
fn call_json(method: &str, args: &[Value]) -> Option<Result<Value, EvalError>> {
    match method {
        "parse" | "from_str" => {
            let text = match args.first()? {
                Value::Str(s) => s.to_string(),
                other => {
                    return Some(Err(EvalError {
                        message: format!("Json.parse needs a Str, got {}", other),
                    }))
                }
            };
            // The TARGET type, when the compiler knew one. Without it the document is
            // read as it stands, which is enough whenever the shapes already line up —
            // roc records are structural, so an object becomes a record and
            // `record.image.title` lands on a field.
            let target = args.get(1).cloned().unwrap_or(Value::Unit);
            let invalid =
                || Value::tag("Err", [Value::tag("InvalidJson", [str_value(text.clone())])]);
            let mut cursor = JsonCursor { bytes: text.as_bytes(), at: 0 };
            Some(match json_read_as(&mut cursor, &target) {
                Err(e) => Err(e),
                Ok(Some(value)) if { cursor.space(); cursor.at >= cursor.bytes.len() } => {
                    Ok(Value::tag("Ok", [value]))
                }
                Ok(_) => Ok(invalid()),
            })
        }
        // The element parser handed to a `parser_for` that delegates through its type
        // parameter: `Elem : a` then `Elem.parser_for(encoding)`. The element's own
        // shape is on the descriptor stack, which is what lets a nested nominal reach
        // ITS `parser_for` rather than arriving as a bare document value.
        "elem_parse" => {
            let text = match args.first()? {
                Value::Str(s) => s.to_string(),
                other => {
                    return Some(Err(EvalError {
                        message: format!("a parser's state is the text left to read, got {}", other),
                    }))
                }
            };
            let want = WRAPPED.with(|stack| stack.borrow().last().cloned()).unwrap_or(Value::Unit);
            let mut cursor = JsonCursor { bytes: text.as_bytes(), at: 0 };
            Some(match json_read_as(&mut cursor, &want) {
                Err(e) => Err(e),
                Ok(Some(value)) => Ok(Value::tag(
                    "Ok",
                    vec![Value::record(vec![
                        ("rest", str_value(text[cursor.at..].to_string())),
                        ("value", value),
                    ])],
                )),
                Ok(None) => Ok(Value::tag(
                    "Err",
                    vec![Value::tag("InvalidJson", [str_value(text)])],
                )),
            })
        }
        // The matching half for `encoder_for`: append the element's JSON to the state.
        "elem_encode" => {
            let value = args.first()?;
            let state = match args.get(1) {
                Some(Value::Str(s)) => s.to_string(),
                _ => String::new(),
            };
            Some(json_encode(value).map(|text| {
                Value::tag("Ok", [str_value(format!("{}{}", state, text))])
            }))
        }
        "to_str" | "encode" => Some(json_encode(args.first()?).map(str_value)),
        // JSON has no syntax for a non-finite number, so roc refuses to render one and
        // says WHICH it was. Every NaN bit pattern classifies the same.
        "to_str_try" => {
            let value = args.first()?;
            let refusal = |f: f64| {
                if f.is_nan() {
                    Some("NaN")
                } else if f.is_infinite() {
                    Some(if f.is_sign_negative() { "NegativeInfinity" } else { "Infinity" })
                } else {
                    None
                }
            };
            let non_finite = match value {
                Value::Float(f) => refusal(*f),
                Value::F32(f) => refusal(*f as f64),
                _ => None,
            };
            Some(match non_finite {
                Some(why) => Ok(Value::tag("Err", [Value::tag(why, Vec::new())])),
                None => json_encode(value).map(|text| Value::tag("Ok", [str_value(text)])),
            })
        }
        _ => None,
    }
}

/// Read a JSON document into the type `target` describes.
///
/// A `Nominal` hands the reading to that type's own `parser_for`, which is how
/// `EncodeDecode`'s `ItemKind` turns the number 1 back into `Text`. Anything else is
/// read as it stands.
fn json_read_as(
    cursor: &mut JsonCursor,
    target: &Value,
) -> Result<Option<Value>, EvalError> {
    cursor.space();
    match target {
        Value::Tag(tag, payload) if &**tag == "List" => {
            let element = payload.first().cloned().unwrap_or(Value::Unit);
            if cursor.bytes.get(cursor.at) != Some(&b'[') {
                return Ok(None);
            }
            cursor.at += 1;
            let mut items = Vec::new();
            cursor.space();
            if cursor.bytes.get(cursor.at) == Some(&b']') {
                cursor.at += 1;
                return Ok(Some(Value::list(items)));
            }
            loop {
                match json_read_as(cursor, &element)? {
                    Some(item) => items.push(item),
                    None => return Ok(None),
                }
                cursor.space();
                match cursor.bytes.get(cursor.at) {
                    Some(b',') => cursor.at += 1,
                    Some(b']') => {
                        cursor.at += 1;
                        return Ok(Some(Value::list(items)));
                    }
                    _ => return Ok(None),
                }
            }
        }
        // An object read FIELD BY FIELD, each against its own descriptor — which is
        // how a field holding a nominal reaches that nominal's `parser_for`. A key the
        // type does not name is read as it stands.
        Value::Tag(tag, payload) if &**tag == "Record" => {
            let shape: Vec<(String, Value)> = payload
                .first()
                .and_then(|v| v.sequence().map(|items| items.to_vec()))
                .unwrap_or_default()
                .into_iter()
                .filter_map(|pair| match pair {
                    Value::Tuple(parts) => match (parts.first(), parts.get(1)) {
                        (Some(Value::Str(name)), Some(desc)) => Some((name.to_string(), desc.clone())),
                        _ => None,
                    },
                    _ => None,
                })
                .collect();
            if cursor.bytes.get(cursor.at) != Some(&b'{') {
                return Ok(None);
            }
            cursor.at += 1;
            let mut fields: Vec<(&'static str, Value)> = Vec::new();
            cursor.space();
            if cursor.bytes.get(cursor.at) == Some(&b'}') {
                cursor.at += 1;
                return Ok(Some(Value::record(fields)));
            }
            loop {
                cursor.space();
                let Some(key) = cursor.string() else { return Ok(None) };
                cursor.space();
                if cursor.bytes.get(cursor.at) != Some(&b':') {
                    return Ok(None);
                }
                cursor.at += 1;
                let want = shape
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, desc)| desc.clone())
                    .unwrap_or(Value::Unit);
                let Some(value) = json_read_as(cursor, &want)? else { return Ok(None) };
                fields.push((crate::memory::string_pool::intern(&key), value));
                cursor.space();
                match cursor.bytes.get(cursor.at) {
                    Some(b',') => cursor.at += 1,
                    Some(b'}') => {
                        cursor.at += 1;
                        return Ok(Some(Value::record(fields)));
                    }
                    _ => return Ok(None),
                }
            }
        }
        Value::Tag(tag, payload) if &**tag == "Nominal" => {
            let Some(Value::Str(name)) = payload.first() else { return Ok(None) };
            let wrapped = payload.get(1).cloned().unwrap_or(Value::Unit);
            custom_parser(name, &wrapped, cursor)
        }
        _ => Ok(cursor.value()),
    }
}

// What the nominal currently being parsed WRAPS, for a `parser_for` that delegates
// through its type parameter (`Elem : a` then `Elem.parser_for(encoding)`).
//
// A stack rather than an argument: the delegation goes through Roc code, which has no
// place to carry a descriptor. Pushed for exactly as long as the nominal's own parser
// runs, so the top is always the element of the innermost one.
thread_local! {
    static WRAPPED: std::cell::RefCell<Vec<Value>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Run a type's own `parser_for` over the rest of the document.
fn custom_parser(
    nominal: &str,
    wrapped: &Value,
    cursor: &mut JsonCursor,
) -> Result<Option<Value>, EvalError> {
    let Some(parser_for) = crate::vm::method_by_name(nominal, "parser_for") else {
        // No parser of its own. The nominal is erased, so the document reads plain —
        // except for a CONTAINER, whose runtime layout is not what it stands for: a
        // `Set` is a bucket record, and its `from_list` is the way in.
        let plain = cursor.value();
        if let (Some(value @ Value::List(_)), Some(from_list)) =
            (plain.clone(), crate::vm::method_by_name(nominal, "from_list"))
        {
            return call_function(from_list, vec![value]).map(Some);
        }
        return Ok(plain);
    };
    WRAPPED.with(|stack| stack.borrow_mut().push(wrapped.clone()));
    let outcome = custom_parser_inner(parser_for, cursor);
    WRAPPED.with(|stack| { stack.borrow_mut().pop(); });
    outcome
}

fn custom_parser_inner(
    parser_for: Value,
    cursor: &mut JsonCursor,
) -> Result<Option<Value>, EvalError> {
    let encoding = Value::tag(JSON_ENCODING, Vec::new());
    let parser = call_function(parser_for, vec![encoding])?;
    let rest = std::str::from_utf8(&cursor.bytes[cursor.at..])
        .map_err(|_| EvalError { message: "JSON must be UTF-8".to_string() })?;
    let parsed = call_function(parser, vec![str_value(rest.to_string())])?;
    match parsed {
        Value::Tag(tag, payload) if &*tag == "Ok" => match payload.first() {
            Some(Value::Record(fields)) => {
                let field = |want: &str| fields.iter().find(|(n, _)| *n == want).map(|(_, v)| v);
                let (Some(value), Some(Value::Str(left))) = (field("value"), field("rest")) else {
                    return Ok(None);
                };
                // The parser reports what is LEFT, which is where the cursor now is.
                cursor.at = cursor.bytes.len() - left.len();
                Ok(Some(value.clone()))
            }
            _ => Ok(None),
        },
        _ => Ok(None),
    }
}

/// Render a value as JSON, letting a type's own `encoder_for` answer for it.
///
/// `Builtin.roc` derives an encoder from the shape through the `Encoding` protocol; a
/// type that wants a different rendering writes `encoder_for` itself, and
/// `EncodeDecode`'s `ItemKind` does — its ten tags encode as the numbers 1 to 10 rather
/// than as their names. Found by SHAPE, the same way a method is found for a value
/// whose nominal the runtime cannot name.
fn json_encode(value: &Value) -> Result<String, EvalError> {
    if let Some(custom) = custom_encoder(value)? {
        return Ok(custom);
    }
    // A `Set` is a `Dict` at run time and a `Dict` is a bucket record inside a
    // `HashMap` tag, so neither renders as itself; roc derives their encoders from the
    // container. A `Set` stands for its ELEMENTS, and `to_list` is the way to them.
    // `Builtin.roc` declares `Set.encoder_for` with no body, so nothing above answers.
    if matches!(value, Value::Record(_) | Value::Tag(..)) {
        let fits = crate::vm::methods_named("to_list", value);
        if let Some((_, to_list)) = fits.into_iter().find(|(owner, _)| *owner == "Set.to_list") {
            let items = call_function(to_list, vec![value.clone()])?;
            return json_encode(&items);
        }
    }
    Ok(match value {
        // A list drives each element's encoder and puts the separators in itself: the
        // element encoders know nothing about where they sit.
        Value::List(_) | Value::Tuple(_) => {
            let items = value.sequence().expect("matched a sequence");
            let mut parts = Vec::with_capacity(items.len());
            for item in items {
                parts.push(json_encode(item)?);
            }
            format!("[{}]", parts.join(","))
        }
        Value::Record(fields) => {
            let mut parts = Vec::with_capacity(fields.len());
            for (name, v) in fields.iter() {
                parts.push(format!("{}:{}", crate::eval::value::quoted(name), json_encode(v)?));
            }
            format!("{{{}}}", parts.join(","))
        }
        other => json_write(other),
    })
}

/// Run a type's own `encoder_for`, if it declares one.
fn custom_encoder(value: &Value) -> Result<Option<String>, EvalError> {
    if !matches!(value, Value::Record(_) | Value::Tag(..) | Value::Tuple(_)) {
        return Ok(None);
    }
    let mut candidates = crate::vm::methods_named("encoder_for", value);
    if candidates.len() != 1 {
        return Ok(None);
    }
    let (_, encoder_for) = candidates.pop().expect("checked len");
    // `encoder_for : encoding -> (value, state -> Try(state, []))`. The encoding is a
    // type witness the body never reads — it dispatches `Encoding.encode_*` on it —
    // so what is passed matters only in that something must be.
    let encoder = call_function(encoder_for, vec![Value::Unit])?;
    // The state is the text built so far. `Builtin.roc` threads a `List(U8)`; a Str is
    // the same thing said in the form this interpreter already has.
    let encoded = call_function(encoder, vec![value.clone(), str_value("")])?;
    match encoded {
        Value::Tag(tag, payload) if &*tag == "Ok" => match payload.first() {
            Some(Value::Str(text)) => Ok(Some(text.to_string())),
            other => Err(EvalError {
                message: format!("an encoder must give back the text so far, got {:?}", other),
            }),
        },
        other => Err(EvalError { message: format!("encoding failed: {}", other) }),
    }
}

/// Render a value as JSON.
fn json_write(value: &Value) -> String {
    match value {
        Value::Str(s) => crate::eval::value::quoted(s),
        Value::Int(n) => n.to_string(),
        Value::Dec(d) => dec_to_string(*d),
        Value::Float(f) => Value::Float(*f).to_string(),
        Value::F32(f) => Value::F32(*f).to_string(),
        Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        Value::Unit => "null".to_string(),
        Value::List(_) | Value::Tuple(_) => {
            let items = value.sequence().expect("matched a sequence");
            let parts: Vec<String> = items.iter().map(json_write).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Record(fields) => {
            let parts: Vec<String> = fields
                .iter()
                .map(|(name, v)| format!("{}:{}", crate::eval::value::quoted(name), json_write(v)))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        // A tag with no payload is its name, which is how roc's own JSON renders a
        // payload-less variant; one WITH a payload renders as the payload.
        Value::Tag(name, payload) => match payload.len() {
            0 => crate::eval::value::quoted(name),
            1 => json_write(&payload[0]),
            _ => {
                let parts: Vec<String> = payload.iter().map(json_write).collect();
                format!("[{}]", parts.join(","))
            }
        },
        other => crate::eval::value::quoted(&other.to_string()),
    }
}

/// A position in a JSON document.
struct JsonCursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl JsonCursor<'_> {
    fn space(&mut self) {
        while self.bytes.get(self.at).is_some_and(|b| b.is_ascii_whitespace()) {
            self.at += 1;
        }
    }

    fn eat(&mut self, word: &str) -> bool {
        if self.bytes[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            return true;
        }
        false
    }

    fn value(&mut self) -> Option<Value> {
        self.space();
        match *self.bytes.get(self.at)? {
            b'"' => self.string().map(str_value),
            b'{' => self.object(),
            b'[' => self.array(),
            b't' => self.eat("true").then_some(Value::Bool(true)),
            b'f' => self.eat("false").then_some(Value::Bool(false)),
            b'n' => self.eat("null").then_some(Value::Unit),
            _ => self.number(),
        }
    }

    fn string(&mut self) -> Option<String> {
        if *self.bytes.get(self.at)? != b'"' {
            return None;
        }
        self.at += 1;
        let mut out = String::new();
        loop {
            match *self.bytes.get(self.at)? {
                b'"' => {
                    self.at += 1;
                    return Some(out);
                }
                b'\\' => {
                    self.at += 1;
                    let escaped = *self.bytes.get(self.at)?;
                    out.push(match escaped {
                        b'n' => '\n',
                        b't' => '\t',
                        b'r' => '\r',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        // `\uXXXX`, the only escape that is not one character.
                        b'u' => {
                            let hex = self.bytes.get(self.at + 1..self.at + 5)?;
                            let code = u32::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
                            self.at += 4;
                            char::from_u32(code)?
                        }
                        other => other as char,
                    });
                    self.at += 1;
                }
                _ => {
                    // A whole character, not a byte: slicing mid-UTF-8 would corrupt it.
                    let rest = std::str::from_utf8(&self.bytes[self.at..]).ok()?;
                    let ch = rest.chars().next()?;
                    out.push(ch);
                    self.at += ch.len_utf8();
                }
            }
        }
    }

    fn number(&mut self) -> Option<Value> {
        let start = self.at;
        if self.bytes.get(self.at) == Some(&b'-') {
            self.at += 1;
        }
        let mut fractional = false;
        while let Some(byte) = self.bytes.get(self.at) {
            match byte {
                b'0'..=b'9' => self.at += 1,
                b'.' | b'e' | b'E' | b'+' | b'-' => {
                    fractional = true;
                    self.at += 1;
                }
                _ => break,
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.at]).ok()?;
        if text.is_empty() {
            return None;
        }
        if fractional {
            // A fractional JSON number is a `Dec`, the type roc defaults one to.
            Some(Value::Dec(dec_from_f64(text.parse::<f64>().ok()?)))
        } else {
            Some(Value::Int(text.parse::<i128>().ok()?))
        }
    }

    fn object(&mut self) -> Option<Value> {
        self.at += 1; // `{`
        let mut fields = Vec::new();
        self.space();
        if self.bytes.get(self.at) == Some(&b'}') {
            self.at += 1;
            return Some(Value::record(fields));
        }
        loop {
            self.space();
            let name = self.string()?;
            // Field names outlive the program, as every other record's do — interned,
            // so a document with ten thousand objects leaks one copy of each name
            // rather than ten thousand.
            let name: &'static str = crate::memory::string_pool::intern(&name);
            self.space();
            if *self.bytes.get(self.at)? != b':' {
                return None;
            }
            self.at += 1;
            fields.push((name, self.value()?));
            self.space();
            match *self.bytes.get(self.at)? {
                b',' => self.at += 1,
                b'}' => {
                    self.at += 1;
                    // Sorted, as `Str.inspect` shows every record.
                    fields.sort_by(|a, b| a.0.cmp(b.0));
                    return Some(Value::record(fields));
                }
                _ => return None,
            }
        }
    }

    fn array(&mut self) -> Option<Value> {
        self.at += 1; // `[`
        let mut items = Vec::new();
        self.space();
        if self.bytes.get(self.at) == Some(&b']') {
            self.at += 1;
            return Some(Value::list(items));
        }
        loop {
            items.push(self.value()?);
            self.space();
            match *self.bytes.get(self.at)? {
                b',' => self.at += 1,
                b']' => {
                    self.at += 1;
                    return Some(Value::list(items));
                }
                _ => return None,
            }
        }
    }
}
