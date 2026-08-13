use serde_json::Value;

#[derive(Debug, Clone)]
pub enum Expr {
    Literal(Value),
    Path(Vec<PathSeg>),
    Unary(UnaryOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
}

#[derive(Debug, Clone)]
pub enum PathSeg {
    Key(String),
    Index(usize),
    Wild,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExprError {
    pub pos: usize,
    pub msg: String,
}

impl ExprError {
    pub fn new(pos: usize, msg: impl Into<String>) -> Self {
        Self { pos, msg: msg.into() }
    }
}

impl std::fmt::Display for ExprError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "expr error at byte {}: {}", self.pos, self.msg)
    }
}

impl std::error::Error for ExprError {}

pub const MAX_PARSE_STEPS: usize = 100_000;
pub const MAX_DEPTH: usize = 64;
