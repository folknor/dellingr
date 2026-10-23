//! The syntax tree the parser builds for one function and code generation
//! consumes.
//!
//! The parser keeps every side effect it had when it emitted bytecode
//! directly - literal-pool insertion, constant folding, upvalue resolution,
//! limit checks - at the same parse events, and records here what each
//! emission point knew: the line stamp of every instruction that can fault
//! or name a call site, and the upvalue-close decisions made at scope and
//! loop exits.
//!
//! The tree's nesting mirrors the parser's recursion. Everything the parser
//! iterates is a flat list here (operator chains of one precedence level,
//! comparison, and/or and concat chains, suffix chains, elseif chains,
//! statement lists), so walking the tree recursively never goes deeper than
//! the parser's charged syntax depth.

use crate::instr::{ArithOp, Builtin, CompareOp};

/// A local variable declaration of one function, by declaration order.
pub(super) type LocalId = u32;

/// Per-declaration facts the parser learns while parsing the function.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct LocalDecl {
    /// A nested function captures this local, so a callback can write it
    /// and its scope's exit must close its upvalue.
    pub(super) captured: bool,
}

/// An expression and whether evaluating it may run script or host code
/// (a call, a method call, an index that can reach `__index`, or `#` that
/// can reach `__len`). Arithmetic, comparison, concatenation and closure
/// creation never do.
#[derive(Debug)]
pub(super) struct Expr {
    pub(super) kind: ExprKind,
    pub(super) may_execute: bool,
}

impl Expr {
    pub(super) fn new(kind: ExprKind) -> Self {
        let may_execute = kind.may_execute();
        Self { kind, may_execute }
    }

    /// Whether this is a call or vararg whose value count follows its use.
    pub(super) fn is_multi(&self) -> bool {
        match &self.kind {
            ExprKind::Vararg { .. } => true,
            ExprKind::Suffixed(suffixed) => {
                matches!(
                    suffixed.suffixes.last(),
                    Some(Suffix::Call(_) | Suffix::Method(_))
                )
            }
            _ => false,
        }
    }
}

#[derive(Debug)]
pub(super) enum ExprKind {
    Nil,
    True,
    False,
    /// A number literal by pool index (folded results included).
    Num(u16),
    /// A string literal by pool index.
    Str(u16),
    /// `...`.
    Vararg {
        line: u32,
    },
    /// A local read, by register and declaration.
    Local {
        reg: u8,
        id: LocalId,
    },
    Upval(u8),
    /// A global read by name literal, with its line stamp.
    Global {
        name: u16,
        line: u32,
    },
    Builtin(Builtin),
    /// A function definition by nested-chunk index.
    Function {
        nested: u8,
        line: u32,
    },
    Table(Box<TableExpr>),
    /// A primary expression followed by field, index, call and method
    /// suffixes.
    Suffixed(Box<Suffixed>),
    /// A parenthesized call or vararg, truncated to one value. Any other
    /// parenthesized expression is its inner expression.
    Paren(Box<Expr>),
    /// `first op1 e1 op2 e2 ...` over `+ -` or over `* / %`, left to right.
    Arith(Box<ArithChain>),
    /// `base ^ exponent`, right-associative through recursion.
    Pow {
        base: Box<Expr>,
        exponent: Box<Expr>,
        line: u32,
    },
    /// `e1 .. e2 .. ...`, reported at its first operator's line.
    Concat {
        operands: Vec<Expr>,
        line: u32,
    },
    /// `first op1 e1 op2 e2 ...` over the comparison operators.
    Compare(Box<CompareChain>),
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not {
        operand: Box<Expr>,
        line: u32,
    },
    Len {
        operand: Box<Expr>,
        line: u32,
    },
    Neg {
        operand: Box<Expr>,
        line: u32,
    },
}

impl ExprKind {
    fn may_execute(&self) -> bool {
        match self {
            Self::Nil
            | Self::True
            | Self::False
            | Self::Num(_)
            | Self::Str(_)
            | Self::Vararg { .. }
            | Self::Local { .. }
            | Self::Upval(_)
            | Self::Global { .. }
            | Self::Builtin(_)
            | Self::Function { .. } => false,
            Self::Table(table) => table.items.iter().any(|item| match item {
                TableItem::Positional(value) | TableItem::Named { value, .. } => value.may_execute,
                TableItem::Keyed { key, value, .. } => key.may_execute || value.may_execute,
                TableItem::Flush { .. } => false,
            }),
            Self::Suffixed(suffixed) => suffixed.base.may_execute || !suffixed.suffixes.is_empty(),
            Self::Paren(inner) => inner.may_execute,
            Self::Arith(chain) => {
                chain.first.may_execute || chain.rest.iter().any(|link| link.rhs.may_execute)
            }
            Self::Pow { base, exponent, .. } => base.may_execute || exponent.may_execute,
            Self::Concat { operands, .. } | Self::And(operands) | Self::Or(operands) => {
                operands.iter().any(|operand| operand.may_execute)
            }
            Self::Compare(chain) => {
                chain.first.may_execute || chain.rest.iter().any(|link| link.rhs.may_execute)
            }
            Self::Not { operand, .. } | Self::Neg { operand, .. } => operand.may_execute,
            Self::Len { .. } => true,
        }
    }
}

#[derive(Debug)]
pub(super) struct ArithChain {
    pub(super) first: Expr,
    pub(super) rest: Vec<ArithLink>,
}

#[derive(Debug)]
pub(super) struct ArithLink {
    pub(super) op: ArithOp,
    pub(super) rhs: Expr,
    /// The operator token's line.
    pub(super) line: u32,
}

#[derive(Debug)]
pub(super) struct CompareChain {
    pub(super) first: Expr,
    pub(super) rest: Vec<CompareLink>,
}

#[derive(Debug)]
pub(super) struct CompareLink {
    pub(super) op: CompareOp,
    pub(super) rhs: Expr,
    /// The operator token's line.
    pub(super) line: u32,
}

#[derive(Debug)]
pub(super) struct Suffixed {
    pub(super) base: Expr,
    pub(super) suffixes: Vec<Suffix>,
}

#[derive(Debug)]
pub(super) enum Suffix {
    /// `.name`, read at `line`.
    Field {
        name: u16,
        line: u32,
    },
    /// `[key]`, read at `line`.
    Index {
        key: Expr,
        line: u32,
    },
    Call(CallSuffix),
    Method(MethodSuffix),
}

#[derive(Debug)]
pub(super) struct CallSuffix {
    pub(super) args: Vec<Expr>,
    /// The line the argument list opens on.
    pub(super) line: u32,
}

#[derive(Debug)]
pub(super) struct MethodSuffix {
    pub(super) name: u16,
    /// The line the method lookup is stamped with.
    pub(super) lookup_line: u32,
    pub(super) args: Vec<Expr>,
    /// The line of the colon.
    pub(super) line: u32,
}

/// How a constructor allocates its table.
#[derive(Clone, Copy, Debug)]
pub(super) enum TableAlloc {
    Plain,
    /// Hash part presized for this many keyed fields.
    Presized(u8),
    /// Keys preinstalled from this template.
    Template(u8),
}

#[derive(Debug)]
pub(super) struct TableExpr {
    pub(super) alloc: TableAlloc,
    /// The line of the opening brace.
    pub(super) line: u32,
    pub(super) items: Vec<TableItem>,
}

#[derive(Debug)]
pub(super) enum TableItem {
    /// A positional value, stored by the next flush.
    Positional(Expr),
    /// `name = value`, written at `line`; `pinned` is the template entry.
    Named {
        name: u16,
        value: Expr,
        line: u32,
        pinned: Option<u8>,
    },
    /// `[key] = value`, written at `line`.
    Keyed { key: Expr, value: Expr, line: u32 },
    /// Store the pending positional values as batch `batch`. `count` is
    /// `None` for the final open batch ending in a call or vararg.
    Flush {
        count: Option<u8>,
        batch: u16,
        line: u32,
    },
}

/// An assignment target.
#[derive(Debug)]
pub(super) enum Place {
    Local(u8),
    Upval(u8),
    Global(u16),
    Builtin(Builtin),
    /// `object.name`.
    Field {
        object: Expr,
        name: u16,
    },
    /// `object[key]`.
    Index {
        object: Expr,
        key: Expr,
    },
}

#[derive(Debug)]
pub(super) struct Block {
    pub(super) stmts: Vec<Stmt>,
    /// Close upvalues from this register when the block ends, decided at the
    /// block's exit like the parser always did.
    pub(super) close: Option<u8>,
}

#[derive(Debug)]
pub(super) enum Stmt {
    /// `local a, b = values`: the `count` new locals start at `first`.
    Local {
        first: u8,
        count: usize,
        values: Vec<Expr>,
    },
    /// `local function name`: the local is in scope inside the body.
    LocalFunction {
        reg: u8,
        nested: u8,
        line: u32,
    },
    /// `targets = values`, stored last target first at `line`.
    Assign {
        targets: Vec<Place>,
        values: Vec<Expr>,
        line: u32,
    },
    /// A call statement (a suffixed expression ending in a call).
    Call(Expr),
    Do(Block),
    While {
        cond: Expr,
        body: Vec<Stmt>,
        /// Close the body locals from this register at the end of each
        /// iteration and before each break.
        body_close: Option<u8>,
        /// The loop's breaks close upvalues from this register.
        break_close: Option<u8>,
        /// The scope around the condition and body.
        scope_close: Option<u8>,
    },
    Repeat {
        body: Vec<Stmt>,
        cond: Expr,
        /// Close the body locals after the condition, before looping.
        body_close: Option<u8>,
        break_close: Option<u8>,
        scope_close: Option<u8>,
    },
    If {
        arms: Vec<IfArm>,
        else_block: Option<Block>,
    },
    NumericFor {
        /// The first of the four loop registers (start, limit, step, var).
        base: u8,
        start: Expr,
        limit: Expr,
        step: Expr,
        body: Vec<Stmt>,
        /// The FORPREP stamp.
        prep_line: u32,
        /// The FORLOOP stamp.
        loop_line: u32,
        body_close: bool,
        break_close: Option<u8>,
        scope_close: Option<u8>,
    },
    GenericFor {
        /// The first of the three hidden registers; the visible variables
        /// follow.
        base: u8,
        vars: u8,
        exprs: Vec<Expr>,
        body: Vec<Stmt>,
        /// The TFORCALL and TFORLOOP stamp.
        call_line: u32,
        body_close: bool,
        break_close: Option<u8>,
        scope_close: Option<u8>,
    },
    /// `function target() end` assigning a closure.
    Function {
        target: Place,
        nested: u8,
        /// The store's stamp.
        line: u32,
    },
    Return {
        values: Vec<Expr>,
        line: u32,
    },
    Break,
}

#[derive(Debug)]
pub(super) struct IfArm {
    pub(super) cond: Expr,
    pub(super) block: Block,
}

/// One function's tree: its body and its local declarations.
#[derive(Debug)]
pub(super) struct FunctionBody {
    pub(super) num_params: u8,
    pub(super) body: Vec<Stmt>,
    /// The implicit return's stamp: the block terminator's line.
    pub(super) end_line: u32,
    pub(super) locals: Vec<LocalDecl>,
}
