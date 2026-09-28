//! The order a top level's definitions are checked in.
//!
//! roc checks a definition after the definitions it reads, not in file order. An
//! unannotated definition's type is whatever checking it finds, so a definition read
//! from above it must be checked first. Otherwise the reader finds no type, and a
//! numeral the reader would have pinned defaults to `Dec` on its own:
//! `scale : U64 -> U64` over a `factor = 3` written below it (B-Teague/rocflight#24).
//! An annotated definition's type is bound before anything is checked, so reading
//! one orders nothing. But its body stays in the order: it can pin what the
//! definitions below it read, as `k : Code` with `k = n` makes the numeral `n` a
//! `Code`.

use super::Type;
use crate::ast::Expr;
use std::collections::HashMap;

/// The order to check `bindings` (name, annotation, value) in: each after every
/// unannotated binding it reads, and otherwise in file order. Bindings that read
/// each other, directly or around a cycle, keep file order among themselves.
///
/// A read is any mention of the name: `factor`, `Code.of`, and, for a method call
/// `x.frob()`, every binding named `<Type>.frob`, since which type's `frob` it is
/// isn't known until it is checked. roc forbids shadowing, so a local can't reuse a
/// top-level name, and binders need no tracking. Over-counting a read can only
/// merge definitions into a cycle, which leaves them in file order, as before.
pub fn check_order(bindings: &[(&'static str, &Option<Type>, &Expr)]) -> Vec<usize> {
    let mut at: HashMap<&str, usize> = HashMap::new();
    let mut by_method: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, (name, annotation, _)) in bindings.iter().enumerate() {
        if annotation.is_some() {
            continue;
        }
        at.entry(name).or_insert(i);
        if let Some((_, method)) = name.rsplit_once('.') {
            by_method.entry(method).or_default().push(i);
        }
    }
    let reads: Vec<Vec<usize>> = bindings
        .iter()
        .map(|(name, _, value)| {
            let owner = name.rsplit_once('.').map(|(owner, _)| owner);
            let mut out = Vec::new();
            mentions(value, owner, &at, &by_method, &mut out);
            out.sort_unstable();
            out.dedup();
            out
        })
        .collect();
    strongly_connected_order(&reads)
}

fn mentions(
    e: &Expr,
    owner: Option<&str>,
    at: &HashMap<&str, usize>,
    by_method: &HashMap<&str, Vec<usize>>,
    out: &mut Vec<usize>,
) {
    match e {
        // A method's siblings are in scope unqualified: `Graph.from_dict`'s body can
        // say `add_edge` for `Graph.add_edge`.
        Expr::Ident(n, _) => {
            out.extend(at.get(n));
            if let Some(owner) = owner {
                out.extend(at.get(format!("{}.{}", owner, n).as_str()));
            }
        }
        Expr::Qualified { module, name, .. } => out.extend(at.get(format!("{}.{}", module, name).as_str())),
        Expr::Dispatch { method, .. } => out.extend(by_method.get(method).into_iter().flatten()),
        _ => {}
    }
    for child in e.children() {
        mentions(child, owner, at, by_method, out);
    }
}

/// Tarjan's strongly connected components over `reads`, flattened. A component comes
/// out after every component it reads, and its members in file order. Started in
/// file order with reads in file order, so where nothing reads forward the answer is
/// file order. Iterative: a top level can have thousands of definitions in a chain.
fn strongly_connected_order(reads: &[Vec<usize>]) -> Vec<usize> {
    let n = reads.len();
    let mut index: Vec<Option<usize>> = vec![None; n];
    let mut low = vec![0; n];
    let mut on_stack = vec![false; n];
    let mut stack = Vec::new();
    let mut next = 0;
    let mut order = Vec::with_capacity(n);
    for root in 0..n {
        if index[root].is_some() {
            continue;
        }
        // (node, how many of its reads have been followed)
        let mut work = vec![(root, 0)];
        index[root] = Some(next);
        low[root] = next;
        next += 1;
        stack.push(root);
        on_stack[root] = true;
        while let Some(&(v, followed)) = work.last() {
            if let Some(&w) = reads[v].get(followed) {
                work.last_mut().expect("v is on the work stack").1 += 1;
                match index[w] {
                    None => {
                        index[w] = Some(next);
                        low[w] = next;
                        next += 1;
                        stack.push(w);
                        on_stack[w] = true;
                        work.push((w, 0));
                    }
                    Some(iw) if on_stack[w] => low[v] = low[v].min(iw),
                    Some(_) => {}
                }
                continue;
            }
            work.pop();
            if let Some(&(parent, _)) = work.last() {
                low[parent] = low[parent].min(low[v]);
            }
            if Some(low[v]) == index[v] {
                let mut component = Vec::new();
                loop {
                    let w = stack.pop().expect("v is on the stack");
                    on_stack[w] = false;
                    component.push(w);
                    if w == v {
                        break;
                    }
                }
                component.sort_unstable();
                order.extend(component);
            }
        }
    }
    order
}

#[cfg(test)]
mod tests {
    use super::strongly_connected_order;

    #[test]
    fn a_forward_read_moves_the_definition_it_reads_up() {
        // 0 reads 2, and 2 reads nothing: 2 goes first.
        assert_eq!(strongly_connected_order(&[vec![2], vec![], vec![]]), vec![2, 0, 1]);
    }

    #[test]
    fn with_no_forward_read_the_order_is_file_order() {
        assert_eq!(strongly_connected_order(&[vec![], vec![0], vec![0, 1]]), vec![0, 1, 2]);
    }

    #[test]
    fn definitions_that_read_each_other_keep_file_order_after_what_they_read() {
        // 0 and 1 read each other, and 1 reads 3.
        assert_eq!(strongly_connected_order(&[vec![1], vec![0, 3], vec![], vec![]]), vec![3, 0, 1, 2]);
    }
}
