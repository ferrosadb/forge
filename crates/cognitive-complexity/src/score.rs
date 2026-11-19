//! Module: Cognitive complexity scoring of a function body (SonarSource metric).
//! Correctness: Correct when every body scores exactly what `complexity` 0.2
//! scored for the same syntax, and syntax that crate could not parse scores by
//! the same rules.
//! Last revised: 2026-11-19
//! Last changed: Ported from the `complexity` 0.2 crate onto syn 2.
//!
//! This is a port of `complexity` 0.2.0 by Ross MacArthur
//! (<https://github.com/rossmacarthur/complexity>, MIT OR Apache-2.0), used
//! here under its Apache-2.0 terms. That crate is built on syn 1, has had no
//! release since 2020, and syn 1 panics on syntax it cannot represent, such as
//! `c"..."` literals. The scoring rules are unchanged; only the syn 2 node
//! shapes differ:
//!
//! - `AssignOp` is now a `Binary` expression; it scored its two operands and
//!   still does, since compound assignment is not a logical operator.
//! - `Box` and `Type` ascription expressions no longer exist.
//! - `let ... else` (new in syn 2) scores its initializer, then its `else`
//!   block one nesting level deeper, like any other block.
//! - A `const { .. }` block expression nests like `unsafe { .. }`.
//! - A statement-position macro scores zero, as an expression macro always has.

use std::{iter, ops};

use syn::{
    Block, Expr, ExprArray, ExprAssign, ExprAsync, ExprAwait, ExprBinary, ExprBlock, ExprBreak,
    ExprCall, ExprCast, ExprClosure, ExprConst, ExprField, ExprForLoop, ExprGroup, ExprIf,
    ExprIndex, ExprLet, ExprLoop, ExprMatch, ExprMethodCall, ExprParen, ExprRange, ExprReference,
    ExprRepeat, ExprReturn, ExprStruct, ExprTry, ExprTryBlock, ExprTuple, ExprUnary, ExprUnsafe,
    ExprWhile, ExprYield, Item, ItemConst, ItemStatic, Local, LocalInit, Stmt,
};

/// Cognitive complexity of a function body, evaluated at nesting level 0.
pub(crate) fn block_complexity(block: &Block) -> u32 {
    eval_block(block, State::default()).0
}

/// Cognitive complexity of a single expression, evaluated at nesting level 0.
#[cfg(test)]
fn expr_complexity(expr: &Expr) -> u32 {
    eval_expr(expr, State::default()).0
}

/// A complexity score.
#[derive(Debug, Clone, Copy)]
struct Index(u32);

impl Index {
    fn zero() -> Self {
        Self(0)
    }

    fn one() -> Self {
        Self(1)
    }

    /// A structural increment: one, plus one per enclosing nesting level.
    fn with_context(state: State) -> Self {
        Self(state.nesting + 1)
    }
}

impl ops::Add for Index {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self(self.0 + other.0)
    }
}

impl iter::Sum<Index> for Index {
    fn sum<I>(iter: I) -> Self
    where
        I: Iterator<Item = Self>,
    {
        iter.fold(Self::zero(), ops::Add::add)
    }
}

/// A logical boolean operator. A run of the same operator scores once.
#[derive(Debug, Clone, Copy, PartialEq)]
enum LogBoolOp {
    Not,
    And,
    Or,
}

impl LogBoolOp {
    fn from_un_op(un_op: syn::UnOp) -> Option<Self> {
        match un_op {
            syn::UnOp::Not(_) => Some(Self::Not),
            _ => None,
        }
    }

    fn from_bin_op(bin_op: &syn::BinOp) -> Option<Self> {
        match bin_op {
            syn::BinOp::And(_) => Some(Self::And),
            syn::BinOp::Or(_) => Some(Self::Or),
            _ => None,
        }
    }

    /// One for the first operator of a run, zero while the operator repeats.
    fn eval_based_on_prev(self, prev: Option<Self>) -> Index {
        match prev {
            Some(prev) if prev == self => Index::zero(),
            _ => Index::one(),
        }
    }
}

/// Where the evaluation is: how deeply nested, and the last logical operator.
#[derive(Debug, Default, Clone, Copy)]
struct State {
    nesting: u32,
    log_bool_op: Option<LogBoolOp>,
}

impl State {
    fn increase_nesting(self) -> Self {
        Self {
            nesting: self.nesting + 1,
            log_bool_op: self.log_bool_op,
        }
    }
}

fn eval_block(block: &Block, state: State) -> Index {
    block.stmts.iter().map(|s| eval_stmt(s, state)).sum()
}

fn eval_stmt(stmt: &Stmt, state: State) -> Index {
    match stmt {
        Stmt::Local(Local {
            init: Some(LocalInit { expr, diverge, .. }),
            ..
        }) => {
            eval_expr(expr, state)
                + diverge
                    .as_ref()
                    .map(|(_, else_expr)| eval_expr(else_expr, state))
                    .unwrap_or_else(Index::zero)
        }
        Stmt::Local(Local { init: None, .. }) => Index::zero(),
        Stmt::Item(item) => eval_item(item, state),
        Stmt::Expr(expr, _) => eval_expr(expr, state),
        Stmt::Macro(_) => Index::zero(),
    }
}

fn eval_item(item: &Item, state: State) -> Index {
    match item {
        Item::Const(ItemConst { expr, .. }) | Item::Static(ItemStatic { expr, .. }) => {
            eval_expr(expr, state)
        }
        _ => Index::zero(),
    }
}

fn eval_expr_unary(expr_unary: &ExprUnary, mut state: State) -> Index {
    let ExprUnary { op, expr, .. } = expr_unary;
    if let Some(current) = LogBoolOp::from_un_op(*op) {
        state.log_bool_op = Some(current);
    }
    eval_expr(expr, state)
}

fn eval_expr_binary(expr_binary: &ExprBinary, mut state: State) -> Index {
    let ExprBinary {
        left, op, right, ..
    } = expr_binary;
    let index = match LogBoolOp::from_bin_op(op) {
        Some(current) => {
            let index = current.eval_based_on_prev(state.log_bool_op);
            state.log_bool_op = Some(current);
            index
        }
        None => Index::zero(),
    };
    index + eval_expr(left, state) + eval_expr(right, state)
}

fn eval_opt_expr(expr: Option<&Expr>, state: State) -> Index {
    expr.map(|e| eval_expr(e, state))
        .unwrap_or_else(Index::zero)
}

fn eval_expr_range(expr_range: &ExprRange, state: State) -> Index {
    let ExprRange { start, end, .. } = expr_range;
    eval_opt_expr(start.as_deref(), state) + eval_opt_expr(end.as_deref(), state)
}

fn eval_expr_if(expr_if: &ExprIf, state: State) -> Index {
    let ExprIf {
        cond,
        then_branch,
        else_branch,
        ..
    } = expr_if;
    Index::with_context(state)
        + eval_expr(cond, state)
        + eval_block(then_branch, state.increase_nesting())
        + else_branch
            .as_ref()
            .map(|(_, expr)| Index::one() + eval_expr(expr, state.increase_nesting()))
            .unwrap_or_else(Index::zero)
}

fn eval_expr_match(expr_match: &ExprMatch, state: State) -> Index {
    let ExprMatch { expr, arms, .. } = expr_match;
    Index::with_context(state)
        + eval_expr(expr, state)
        + arms
            .iter()
            .map(|arm| {
                arm.guard
                    .as_ref()
                    .map(|(_, guard)| eval_expr(guard, state))
                    .unwrap_or_else(Index::zero)
                    + eval_expr(&arm.body, state.increase_nesting())
            })
            .sum::<Index>()
}

fn eval_expr_for_loop(expr_for_loop: &ExprForLoop, state: State) -> Index {
    let ExprForLoop { expr, body, .. } = expr_for_loop;
    Index::with_context(state) + eval_expr(expr, state) + eval_block(body, state.increase_nesting())
}

fn eval_expr_while(expr_while: &ExprWhile, state: State) -> Index {
    let ExprWhile { cond, body, .. } = expr_while;
    Index::with_context(state) + eval_expr(cond, state) + eval_block(body, state.increase_nesting())
}

fn eval_expr_struct(expr_struct: &ExprStruct, state: State) -> Index {
    let ExprStruct { fields, rest, .. } = expr_struct;
    fields
        .iter()
        .map(|field| eval_expr(&field.expr, state))
        .sum::<Index>()
        + eval_opt_expr(rest.as_deref(), state)
}

fn eval_expr_call(expr_call: &ExprCall, state: State) -> Index {
    let ExprCall { func, args, .. } = expr_call;
    eval_expr(func, state) + args.iter().map(|a| eval_expr(a, state)).sum::<Index>()
}

fn eval_expr_method_call(expr_method_call: &ExprMethodCall, state: State) -> Index {
    let ExprMethodCall { receiver, args, .. } = expr_method_call;
    eval_expr(receiver, state) + args.iter().map(|a| eval_expr(a, state)).sum::<Index>()
}

/// Expressions that open a nested block: the block scores one level deeper.
fn nested_block(expr: &Expr) -> Option<&Block> {
    match expr {
        Expr::Async(ExprAsync { block, .. })
        | Expr::Block(ExprBlock { block, .. })
        | Expr::Const(ExprConst { block, .. })
        | Expr::Loop(ExprLoop { body: block, .. })
        | Expr::TryBlock(ExprTryBlock { block, .. })
        | Expr::Unsafe(ExprUnsafe { block, .. }) => Some(block),
        _ => None,
    }
}

/// Expressions that wrap exactly one inner expression and add nothing.
fn single_inner(expr: &Expr) -> Option<&Expr> {
    match expr {
        Expr::Await(ExprAwait { base: inner, .. })
        | Expr::Break(ExprBreak {
            expr: Some(inner), ..
        })
        | Expr::Cast(ExprCast { expr: inner, .. })
        | Expr::Closure(ExprClosure { body: inner, .. })
        | Expr::Field(ExprField { base: inner, .. })
        | Expr::Group(ExprGroup { expr: inner, .. })
        | Expr::Let(ExprLet { expr: inner, .. })
        | Expr::Paren(ExprParen { expr: inner, .. })
        | Expr::Reference(ExprReference { expr: inner, .. })
        | Expr::Return(ExprReturn {
            expr: Some(inner), ..
        })
        | Expr::Try(ExprTry { expr: inner, .. })
        | Expr::Yield(ExprYield {
            expr: Some(inner), ..
        }) => Some(inner),
        _ => None,
    }
}

/// Complexity of an expression: nesting structures and breaks in linear flow
/// add to it; everything else contributes only what it contains.
fn eval_expr(expr: &Expr, state: State) -> Index {
    if let Some(block) = nested_block(expr) {
        return eval_block(block, state.increase_nesting());
    }
    if let Some(inner) = single_inner(expr) {
        return eval_expr(inner, state);
    }
    match expr {
        Expr::Array(ExprArray { elems, .. }) | Expr::Tuple(ExprTuple { elems, .. }) => {
            elems.iter().map(|e| eval_expr(e, state)).sum()
        }
        Expr::Unary(expr_unary) => eval_expr_unary(expr_unary, state),
        Expr::Binary(expr_binary) => eval_expr_binary(expr_binary, state),
        Expr::Assign(ExprAssign { left, right, .. })
        | Expr::Index(ExprIndex {
            expr: left,
            index: right,
            ..
        })
        | Expr::Repeat(ExprRepeat {
            expr: left,
            len: right,
            ..
        }) => eval_expr(left, state) + eval_expr(right, state),
        Expr::Range(expr_range) => eval_expr_range(expr_range, state),
        Expr::ForLoop(expr_for_loop) => eval_expr_for_loop(expr_for_loop, state),
        Expr::While(expr_while) => eval_expr_while(expr_while, state),
        Expr::If(expr_if) => eval_expr_if(expr_if, state),
        Expr::Match(expr_match) => eval_expr_match(expr_match, state),
        Expr::Continue(_) | Expr::Break(_) => Index::one(),
        Expr::Struct(expr_struct) => eval_expr_struct(expr_struct, state),
        Expr::Call(expr_call) => eval_expr_call(expr_call, state),
        Expr::MethodCall(expr_method_call) => eval_expr_method_call(expr_method_call, state),
        // Literals, paths, macros, `return`/`yield` without a value, `_` and
        // unparsed verbatim tokens contribute nothing. `Expr` is
        // non-exhaustive, so this arm also covers syntax added after syn 2.0.
        _ => Index::zero(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    // Upstream `complexity` 0.2.0's own unit tests, unchanged.

    #[test]
    fn if_statement() {
        let expr: Expr = parse_quote! {
            if true {             // +1
                println!("test");
            }
        };
        assert_eq!(expr_complexity(&expr), 1);
    }

    #[test]
    fn if_statement_nesting_increment() {
        let expr: Expr = parse_quote! {
            if true {                 // +1
                if true {             // +2 (nesting = 1)
                    println!("test");
                }
            }
        };
        assert_eq!(expr_complexity(&expr), 3);
    }

    #[test]
    fn if_else_statement_no_nesting_increment() {
        let expr: Expr = parse_quote! {
            if true {                 // +1
                if true {             // +2 (nesting = 1)
                    println!("test");
                } else {              // +1
                    println!("test");
                }
            }
        };
        assert_eq!(expr_complexity(&expr), 4);
    }

    #[test]
    fn for_loop() {
        let expr: Expr = parse_quote! {
            for element in iterable { // +1
                if true {             // +2 (nesting = 1)
                    println!("test");
                }
            }
        };
        assert_eq!(expr_complexity(&expr), 3);
    }

    #[test]
    fn for_loop_nesting_increment() {
        let expr: Expr = parse_quote! {
            if true {                     // +1
                for element in iterable { // +2 (nesting = 1)
                    println!("test");
                }
            }
        };
        assert_eq!(expr_complexity(&expr), 3);
    }

    #[test]
    fn while_loop() {
        let expr: Expr = parse_quote! {
            while true {              // +1
                if true {             // +2 (nesting = 1)
                    println!("test");
                }
            }
        };
        assert_eq!(expr_complexity(&expr), 3);
    }

    #[test]
    fn while_loop_nesting_increment() {
        let expr: Expr = parse_quote! {
            if true {                 // +1
                while true {          // +2 (nesting = 1)
                    println!("test");
                }
            }
        };
        assert_eq!(expr_complexity(&expr), 3);
    }

    #[test]
    fn match_statement_nesting_increment() {
        let expr: Expr = parse_quote! {
            if true {                          // +1
                match true {                   // +2 (nesting = 1)
                    true => println!("test"),
                    false => println!("test"),
                }
            }
        };
        assert_eq!(expr_complexity(&expr), 3);
    }

    #[test]
    fn logical_boolean_operators_same() {
        for expr in [
            parse_quote! { x && y },
            parse_quote! { x && y && z },
            parse_quote! { w && x && y && z },
            parse_quote! { x || y },
            parse_quote! { x || y || z },
            parse_quote! { w || x || y || z },
        ] {
            assert_eq!(expr_complexity(&expr), 1);
        }
    }

    #[test]
    fn logical_boolean_operators_changing() {
        let expr: Expr = parse_quote! { w && x || y || z };
        assert_eq!(expr_complexity(&expr), 2);
        let expr: Expr = parse_quote! { w && x && y || z };
        assert_eq!(expr_complexity(&expr), 2);
        let expr: Expr = parse_quote! { w && x || y && z };
        assert_eq!(expr_complexity(&expr), 3);
    }

    #[test]
    fn logical_boolean_operators_not_operator() {
        let expr: Expr = parse_quote! { !a && !b };
        assert_eq!(expr_complexity(&expr), 1);
        let expr: Expr = parse_quote! { a && !(b && c) };
        assert_eq!(expr_complexity(&expr), 2);
        let expr: Expr = parse_quote! { !(a || b) && !(c || d) };
        assert_eq!(expr_complexity(&expr), 3);
    }

    // Syntax syn 1 could not parse.

    #[test]
    fn let_else_scores_its_else_block_one_level_deeper() {
        let block: Block = parse_quote! {{
            let Some(x) = value else {     // block: nesting 1
                if bad { return; }         // +2 (nesting = 1)
                return;
            };
        }};
        assert_eq!(block_complexity(&block), 2);
    }

    #[test]
    fn a_c_string_literal_scores_like_any_literal() {
        let block: Block = parse_quote! {{
            let name = c"forge";
            if ready { use_it(name); }     // +1
        }};
        assert_eq!(block_complexity(&block), 1);
    }

    #[test]
    fn a_const_block_nests_like_an_unsafe_block() {
        let block: Block = parse_quote! {{
            let n = const { if A { 1 } else { 2 } }; // +2 (nesting = 1), else +1
        }};
        assert_eq!(block_complexity(&block), 3);
    }

    #[test]
    fn compound_assignment_scores_only_its_operands() {
        let block: Block = parse_quote! {{
            total += if a && b { 1 } else { 0 }; // && +1, if +1, else +1
        }};
        assert_eq!(block_complexity(&block), 3);
    }
}
