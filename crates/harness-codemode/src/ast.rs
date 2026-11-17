//! The syntax tree of a codemode script.

use std::rc::Rc;

pub type Name = Rc<str>;

#[derive(Debug)]
pub struct Stmt {
    pub line: u32,
    pub kind: StmtKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VarKind {
    Var,
    Let,
    Const,
}

#[derive(Debug)]
pub struct Declarator {
    pub target: Pattern,
    pub init: Option<Expr>,
}

#[derive(Debug)]
pub struct SwitchCase {
    pub test: Option<Expr>,
    pub body: Vec<Stmt>,
}

#[derive(Debug)]
pub enum ForInit {
    Var(VarKind, Vec<Declarator>),
    Expr(Expr),
}

#[derive(Debug)]
pub enum ForHead {
    Var(VarKind, Pattern),
    Target(Pattern),
}

#[derive(Debug)]
pub enum StmtKind {
    Var(VarKind, Vec<Declarator>),
    Function(Name, Rc<Function>),
    Expr(Expr),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    For {
        init: Option<ForInit>,
        test: Option<Expr>,
        update: Option<Expr>,
        body: Box<Stmt>,
    },
    /// `for (head of iterable)`, or `for (head in object)` when `keys`.
    ForEach {
        head: ForHead,
        iterable: Expr,
        body: Box<Stmt>,
        keys: bool,
    },
    While(Expr, Box<Stmt>),
    DoWhile(Box<Stmt>, Expr),
    Block(Vec<Stmt>),
    Return(Option<Expr>),
    Break(Option<Name>),
    Continue(Option<Name>),
    Throw(Expr),
    Try {
        block: Vec<Stmt>,
        param: Option<Pattern>,
        handler: Option<Vec<Stmt>>,
        finalizer: Option<Vec<Stmt>>,
    },
    Switch(Expr, Vec<SwitchCase>),
    Labeled(Name, Box<Stmt>),
    Empty,
}

#[derive(Debug)]
pub enum FunctionBody {
    Expr(Box<Expr>),
    Block(Vec<Stmt>),
}

#[derive(Debug)]
pub struct Param {
    pub target: Pattern,
    pub default: Option<Expr>,
}

#[derive(Debug)]
pub struct Function {
    pub name: Option<Name>,
    pub params: Vec<Param>,
    pub rest: Option<Pattern>,
    pub body: FunctionBody,
    pub arrow: bool,
    pub is_async: bool,
    pub line: u32,
}

#[derive(Debug)]
pub enum PropKey {
    Name(Name),
    Computed(Expr),
}

#[derive(Debug)]
pub enum Property {
    Value(PropKey, Expr),
    Shorthand(Name),
    Spread(Expr),
}

#[derive(Debug)]
pub enum Element {
    Expr(Expr),
    Spread(Expr),
    Hole,
}

#[derive(Debug)]
pub struct PatternProp {
    pub key: PropKey,
    pub target: Pattern,
    pub default: Option<Expr>,
}

#[derive(Debug)]
pub struct PatternItem {
    pub target: Pattern,
    pub default: Option<Expr>,
}

#[derive(Debug)]
pub enum Pattern {
    Ident(Name),
    Object {
        props: Vec<PatternProp>,
        rest: Option<Name>,
    },
    Array {
        items: Vec<Option<PatternItem>>,
        rest: Option<Box<Pattern>>,
    },
    /// An assignment target that is not a name: `obj.key`, `list[i]`.
    Member(Box<Expr>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Not,
    Neg,
    Plus,
    BitNot,
    Typeof,
    Void,
    Delete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Eq,
    NotEq,
    StrictEq,
    StrictNotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    UShr,
    In,
    InstanceOf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogicalOp {
    And,
    Or,
    Nullish,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignOp {
    Assign,
    Binary(BinaryOp),
    Logical(LogicalOp),
}

#[derive(Debug)]
pub enum MemberKey {
    Name(Name),
    Computed(Box<Expr>),
}

#[derive(Debug)]
pub enum Expr {
    Number(f64),
    Str(Rc<str>),
    Template(Vec<Rc<str>>, Vec<Expr>),
    Regex(Rc<str>, Rc<str>),
    Ident(Name),
    This,
    Bool(bool),
    Null,
    Array(Vec<Element>),
    Object(Vec<Property>),
    Function(Rc<Function>),
    Unary(UnaryOp, Box<Expr>),
    Update {
        increment: bool,
        prefix: bool,
        target: Box<Expr>,
    },
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    Logical(LogicalOp, Box<Expr>, Box<Expr>),
    Assign(AssignOp, Box<Pattern>, Box<Expr>),
    Conditional(Box<Expr>, Box<Expr>, Box<Expr>),
    Call {
        callee: Box<Expr>,
        args: Vec<Element>,
        optional: bool,
    },
    New(Box<Expr>, Vec<Element>),
    Member {
        object: Box<Expr>,
        key: MemberKey,
        optional: bool,
    },
    /// The boundary of an optional chain: a `?.` on a nullish value makes
    /// the whole chain `undefined`.
    Chain(Box<Expr>),
    Sequence(Vec<Expr>),
    Await(Box<Expr>),
}
