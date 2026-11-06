//! A linearizability checker for a register, for fault-injection tests.
//!
//! Clients record each operation's invocation and completion times and
//! its outcome: a write of a value, or a read that saw a value. The
//! history is linearizable if every operation can be placed at one instant
//! between its invocation and its completion so that each read sees the
//! latest write placed before it. Writes whose outcome is unknown (a
//! timeout, a lost connection) may have happened at any time after their
//! invocation, or never; reads without an answer tell nothing and are left
//! out.
//!
//! The search is Wing and Gong's, with Lowe's memo of visited
//! (linearized set, register value) pairs.

use std::collections::HashSet;

/// What an operation did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Wrote this value (values are distinct and never 0).
    Write(u64),
    /// Read this value (0 for the register's initial, empty value).
    Read(u64),
}

/// One operation of a history.
#[derive(Clone, Copy, Debug)]
pub struct Op {
    pub kind: Kind,
    /// When the client sent it, in any monotonic unit.
    pub invoke: u64,
    /// When the answer came; `None` for a write with no known outcome.
    pub complete: Option<u64>,
}

/// Whether `history` is linearizable for a register that starts empty (0).
pub fn linearizable(history: &[Op]) -> bool {
    // Writes with no outcome complete after everything; reads with no
    // answer are not in the history. A write with no outcome whose value
    // no read saw is as good as placed last, so it is left out too (it
    // would only widen the search).
    let read: HashSet<u64> = history
        .iter()
        .filter_map(|op| match (op.kind, op.complete) {
            (Kind::Read(v), Some(_)) => Some(v),
            _ => None,
        })
        .collect();
    let ops: Vec<&Op> = history
        .iter()
        .filter(|op| match (op.kind, op.complete) {
            (_, Some(_)) => true,
            (Kind::Write(v), None) => read.contains(&v),
            (Kind::Read(_), None) => false,
        })
        .collect();
    if ops.is_empty() {
        return true;
    }
    // Events in time order; a call comes before a return at the same time.
    let mut events: Vec<(u64, bool, usize)> = Vec::with_capacity(ops.len() * 2);
    for (at, op) in ops.iter().enumerate() {
        events.push((op.invoke, true, at));
        events.push((op.complete.unwrap_or(u64::MAX), false, at));
    }
    events.sort_by_key(|&(time, call, _)| (time, !call));
    // A doubly linked list over the events, with a head at 0.
    let n = events.len();
    let mut next: Vec<usize> = (1..=n + 1).collect();
    let mut prev: Vec<usize> = (0..=n).map(|i| i.saturating_sub(1)).collect();
    next[n] = usize::MAX;
    let mut return_of = vec![0usize; ops.len()];
    for (slot, &(_, call, op)) in events.iter().enumerate() {
        if !call {
            return_of[op] = slot + 1;
        }
    }
    let event = |slot: usize| events[slot - 1];
    let lift = |next: &mut Vec<usize>, prev: &mut Vec<usize>, slot: usize| {
        let (p, q) = (prev[slot], next[slot]);
        next[p] = q;
        if q != usize::MAX {
            prev[q] = p;
        }
    };
    let unlift = |next: &mut Vec<usize>, prev: &mut Vec<usize>, slot: usize| {
        let (p, q) = (prev[slot], next[slot]);
        next[p] = slot;
        if q != usize::MAX {
            prev[q] = slot;
        }
    };

    let words = ops.len().div_ceil(64);
    let mut linearized = vec![0u64; words];
    let mut seen: HashSet<(Vec<u64>, u64)> = HashSet::new();
    let mut stack: Vec<(usize, u64)> = Vec::new();
    let mut value = 0u64;
    let mut slot = next[0];
    while next[0] != usize::MAX {
        if slot == usize::MAX {
            // Nothing more can go first: undo the last choice.
            let Some((call, before)) = stack.pop() else {
                return false;
            };
            let op = event(call).2;
            value = before;
            linearized[op / 64] &= !(1 << (op % 64));
            unlift(&mut next, &mut prev, return_of[op]);
            unlift(&mut next, &mut prev, call);
            slot = next[call];
            continue;
        }
        let (_, call, op) = event(slot);
        if call {
            let (legal, after) = match ops[op].kind {
                Kind::Write(v) => (true, v),
                Kind::Read(v) => (v == value, value),
            };
            if legal {
                linearized[op / 64] |= 1 << (op % 64);
                if seen.insert((linearized.clone(), after)) {
                    stack.push((slot, value));
                    value = after;
                    lift(&mut next, &mut prev, slot);
                    lift(&mut next, &mut prev, return_of[op]);
                    slot = next[0];
                    continue;
                }
                linearized[op / 64] &= !(1 << (op % 64));
            }
            slot = next[slot];
        } else {
            // An operation returned before it could be placed: undo.
            let Some((call, before)) = stack.pop() else {
                return false;
            };
            let op = event(call).2;
            value = before;
            linearized[op / 64] &= !(1 << (op % 64));
            unlift(&mut next, &mut prev, return_of[op]);
            unlift(&mut next, &mut prev, call);
            slot = next[call];
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(kind: Kind, invoke: u64, complete: Option<u64>) -> Op {
        Op {
            kind,
            invoke,
            complete,
        }
    }

    #[test]
    fn sequential_histories() {
        assert!(linearizable(&[]));
        assert!(linearizable(&[
            op(Kind::Read(0), 0, Some(1)),
            op(Kind::Write(1), 2, Some(3)),
            op(Kind::Read(1), 4, Some(5)),
        ]));
        // A read sees a value that was overwritten before it began.
        assert!(!linearizable(&[
            op(Kind::Write(1), 0, Some(1)),
            op(Kind::Write(2), 2, Some(3)),
            op(Kind::Read(1), 4, Some(5)),
        ]));
        // A read sees a value nobody wrote.
        assert!(!linearizable(&[op(Kind::Read(7), 0, Some(1))]));
    }

    #[test]
    fn concurrent_histories() {
        // Overlapping writes: a read may see either, but two later reads
        // must agree on the order.
        assert!(linearizable(&[
            op(Kind::Write(1), 0, Some(10)),
            op(Kind::Write(2), 1, Some(9)),
            op(Kind::Read(2), 11, Some(12)),
            op(Kind::Read(2), 13, Some(14)),
        ]));
        assert!(!linearizable(&[
            op(Kind::Write(1), 0, Some(10)),
            op(Kind::Write(2), 1, Some(9)),
            op(Kind::Read(2), 11, Some(12)),
            op(Kind::Read(1), 13, Some(14)),
        ]));
        // A read during a write may see the old or the new value, but the
        // old one cannot come back after the new one was seen.
        assert!(linearizable(&[
            op(Kind::Write(1), 0, Some(1)),
            op(Kind::Write(2), 2, Some(20)),
            op(Kind::Read(1), 3, Some(4)),
            op(Kind::Read(2), 5, Some(6)),
        ]));
        assert!(!linearizable(&[
            op(Kind::Write(1), 0, Some(1)),
            op(Kind::Write(2), 2, Some(20)),
            op(Kind::Read(2), 3, Some(4)),
            op(Kind::Read(1), 5, Some(6)),
        ]));
    }

    #[test]
    fn writes_with_unknown_outcome() {
        // It may have happened, late...
        assert!(linearizable(&[
            op(Kind::Write(1), 0, None),
            op(Kind::Read(0), 1, Some(2)),
            op(Kind::Read(1), 30, Some(31)),
        ]));
        // ...or never.
        assert!(linearizable(&[
            op(Kind::Write(1), 0, None),
            op(Kind::Read(0), 30, Some(31)),
        ]));
        // But not before it was sent.
        assert!(!linearizable(&[
            op(Kind::Read(1), 0, Some(1)),
            op(Kind::Write(1), 2, None),
        ]));
    }
}
