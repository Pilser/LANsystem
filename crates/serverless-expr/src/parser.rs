use crate::ast::{BinOp, ExprError, UnaryOp, MAX_DEPTH, MAX_PARSE_STEPS};
use std::iter::Peekable;
use std::str::Chars;

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Num(String),
    Str(String),
    Ident(String),
    Path(String),
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Not,
    Question,
    Colon,
    Comma,
    LParen,
    RParen,
    LBracket,
    RBracket,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Num,
    Str,
    Ident,
    Path,
    Op,
    Punct,
    End,
}

pub struct Lexer<'a> {
    chars: Peekable<Chars<'a>>,
    pos: usize,
    src: &'a str,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Self { chars: src.chars().peekable(), pos: 0, src }
    }

    fn peek(&mut self) -> Option<char> {
        self.chars.peek().copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.chars.next()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn is_ident_start(c: char) -> bool {
        c.is_ascii_alphabetic() || c == '_'
    }

    fn is_ident_char(c: char) -> bool {
        c.is_ascii_alphanumeric() || c == '_'
    }

    fn is_path_char(c: char) -> bool {
        c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '[' | ']' | '*')
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.bump();
            } else {
                break;
            }
        }
    }

    pub fn next_token(&mut self) -> Result<Token, ExprError> {
        self.skip_ws();
        let Some(c) = self.peek() else {
            return Ok(Token::End);
        };
        match c {
            '(' => { self.bump(); Ok(Token::LParen) }
            ')' => { self.bump(); Ok(Token::RParen) }
            '[' => { self.bump(); Ok(Token::LBracket) }
            ']' => { self.bump(); Ok(Token::RBracket) }
            '?' => { self.bump(); Ok(Token::Question) }
            ':' => { self.bump(); Ok(Token::Colon) }
            ',' => { self.bump(); Ok(Token::Comma) }
            '+' => { self.bump(); Ok(Token::Plus) }
            '*' => { self.bump(); Ok(Token::Star) }
            '/' => { self.bump(); Ok(Token::Slash) }
            '%' => { self.bump(); Ok(Token::Percent) }
            '-' => {
                if self.is_digit_after_dash() {
                    self.lex_number()
                } else {
                    self.bump(); Ok(Token::Minus)
                }
            }
            '!' => {
                self.bump();
                let t = if self.peek() == Some('=') { self.bump(); Token::Ne } else { Token::Not };
                Ok(t)
            }
            '=' => {
                self.bump();
                if self.peek() == Some('=') { self.bump(); Ok(Token::Eq) } else { Err(ExprError::new(self.pos, "expected '=='")) }
            }
            '<' => {
                self.bump();
                let t = if self.peek() == Some('=') { self.bump(); Token::Le } else { Token::Lt };
                Ok(t)
            }
            '>' => {
                self.bump();
                let t = if self.peek() == Some('=') { self.bump(); Token::Ge } else { Token::Gt };
                Ok(t)
            }
            '&' => {
                self.bump();
                if self.peek() == Some('&') { self.bump(); Ok(Token::And) } else { Err(ExprError::new(self.pos, "expected '&&'")) }
            }
            '|' => {
                self.bump();
                if self.peek() == Some('|') { self.bump(); Ok(Token::Or) } else { Err(ExprError::new(self.pos, "expected '||'")) }
            }
            '$' => self.lex_path(),
            '"' | '\'' => self.lex_string(c),
            '0'..='9' => self.lex_number(),
            _ if Self::is_ident_start(c) => self.lex_ident(),
            _ => Err(ExprError::new(self.pos, format!("unexpected character '{c}'"))),
        }
    }

    fn is_digit_after_dash(&mut self) -> bool {
        self.chars.clone().nth(1).map(|c| c.is_ascii_digit() || c == '.').unwrap_or(false)
    }

    fn lex_ident(&mut self) -> Result<Token, ExprError> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if Self::is_ident_char(c) { self.bump(); } else { break; }
        }
        Ok(Token::Ident(self.src_slice(start).to_string()))
    }

    fn lex_path(&mut self) -> Result<Token, ExprError> {
        let start = self.pos;
        self.bump();
        if self.peek() == Some('.') || self.peek() == Some('[') {
            while let Some(c) = self.peek() {
                if Self::is_path_char(c) { self.bump(); } else { break; }
            }
        }
        Ok(Token::Path(self.src_slice(start).to_string()))
    }

    fn lex_string(&mut self, quote: char) -> Result<Token, ExprError> {
        self.bump();
        let mut out = String::new();
        loop {
            let Some(c) = self.bump() else {
                return Err(ExprError::new(self.pos, "unterminated string literal"));
            };
            if c == quote {
                break;
            }
            if c == '\\' {
                let Some(esc) = self.bump() else {
                    return Err(ExprError::new(self.pos, "unterminated escape"));
                };
                out.push(match esc {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    '"' => '"',
                    '\'' => '\'',
                    '\\' => '\\',
                    _ => return Err(ExprError::new(self.pos, format!("unknown escape '\\{esc}'"))),
                });
            } else {
                out.push(c);
            }
        }
        Ok(Token::Str(out))
    }

    fn lex_number(&mut self) -> Result<Token, ExprError> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || c == '.' { self.bump(); } else { break; }
        }
        Ok(Token::Num(self.src_slice(start).to_string()))
    }

    fn src_slice(&self, start: usize) -> &str {
        let len = self.pos - start;
        &self.src[start..start + len]
    }
}

pub struct Parser {
    tokens: Vec<Token>,
    tok_pos: Vec<usize>,
    idx: usize,
    steps: usize,
}

fn tok_kind(t: &Token) -> TokenKind {
    match t {
        Token::Num(_) => TokenKind::Num,
        Token::Str(_) => TokenKind::Str,
        Token::Ident(_) => TokenKind::Ident,
        Token::Path(_) => TokenKind::Path,
        Token::End => TokenKind::End,
        _ => TokenKind::Op,
    }
}

impl Parser {
    pub fn new(src: &str) -> Result<Self, ExprError> {
        let mut lexer = Lexer::new(src);
        let mut tokens = Vec::new();
        let mut tok_pos = Vec::new();
        loop {
            let t = lexer.next_token()?;
            let p = lexer.pos;
            let is_end = t == Token::End;
            tokens.push(t);
            tok_pos.push(p);
            if is_end {
                break;
            }
        }
        Ok(Self { tokens, tok_pos, idx: 0, steps: 0 })
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.idx]
    }

    fn peek_kind(&self) -> TokenKind {
        tok_kind(self.peek())
    }

    fn next(&mut self) -> Token {
        let t = self.tokens[self.idx].clone();
        if !matches!(t, Token::End) {
            self.idx += 1;
        }
        t
    }

    fn step(&mut self, msg: &str) -> Result<(), ExprError> {
        self.steps += 1;
        if self.steps > MAX_PARSE_STEPS {
            return Err(ExprError::new(self.tok_pos[self.idx], "expression too complex"));
        }
        if self.idx > MAX_DEPTH * 8 {
            return Err(ExprError::new(self.tok_pos[self.idx], format!("{msg} expression too deep")));
        }
        Ok(())
    }

    pub fn parse(mut self) -> Result<crate::ast::Expr, ExprError> {
        let e = self.parse_ternary(0)?;
        if self.peek_kind() != TokenKind::End {
            return Err(ExprError::new(self.tok_pos[self.idx], "unexpected trailing token"));
        }
        Ok(e)
    }

    fn parse_ternary(&mut self, depth: usize) -> Result<crate::ast::Expr, ExprError> {
        self.step("ternary")?;
        if depth > MAX_DEPTH {
            return Err(ExprError::new(self.tok_pos[self.idx], "expression too deeply nested"));
        }
        let cond = self.parse_or(depth + 1)?;
        if self.peek_kind() == TokenKind::Op && *self.peek() == Token::Question {
            self.next();
            let then_b = self.parse_ternary(depth + 1)?;
            if *self.peek() != Token::Colon {
                return Err(ExprError::new(self.tok_pos[self.idx], "expected ':' in ternary"));
            }
            self.next();
            let else_b = self.parse_ternary(depth + 1)?;
            return Ok(crate::ast::Expr::Ternary(Box::new(cond), Box::new(then_b), Box::new(else_b)));
        }
        Ok(cond)
    }

    fn parse_or(&mut self, depth: usize) -> Result<crate::ast::Expr, ExprError> {
        let mut left = self.parse_and(depth + 1)?;
        while self.peek_kind() == TokenKind::Op && *self.peek() == Token::Or {
            self.next();
            let right = self.parse_and(depth + 1)?;
            left = crate::ast::Expr::Binary(BinOp::Or, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self, depth: usize) -> Result<crate::ast::Expr, ExprError> {
        let mut left = self.parse_comparison(depth + 1)?;
        while self.peek_kind() == TokenKind::Op && *self.peek() == Token::And {
            self.next();
            let right = self.parse_comparison(depth + 1)?;
            left = crate::ast::Expr::Binary(BinOp::And, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_comparison(&mut self, depth: usize) -> Result<crate::ast::Expr, ExprError> {
        let mut left = self.parse_additive(depth + 1)?;
        loop {
            let op = match self.peek() {
                Token::Eq => BinOp::Eq,
                Token::Ne => BinOp::Ne,
                Token::Lt => BinOp::Lt,
                Token::Le => BinOp::Le,
                Token::Gt => BinOp::Gt,
                Token::Ge => BinOp::Ge,
                _ => break,
            };
            self.next();
            let right = self.parse_additive(depth + 1)?;
            left = crate::ast::Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_additive(&mut self, depth: usize) -> Result<crate::ast::Expr, ExprError> {
        let mut left = self.parse_multiplicative(depth + 1)?;
        loop {
            let op = match self.peek() {
                Token::Plus => BinOp::Add,
                Token::Minus => BinOp::Sub,
                _ => break,
            };
            self.next();
            let right = self.parse_multiplicative(depth + 1)?;
            left = crate::ast::Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_multiplicative(&mut self, depth: usize) -> Result<crate::ast::Expr, ExprError> {
        let mut left = self.parse_unary(depth + 1)?;
        loop {
            let op = match self.peek() {
                Token::Star => BinOp::Mul,
                Token::Slash => BinOp::Div,
                Token::Percent => BinOp::Mod,
                _ => break,
            };
            self.next();
            let right = self.parse_unary(depth + 1)?;
            left = crate::ast::Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_unary(&mut self, depth: usize) -> Result<crate::ast::Expr, ExprError> {
        self.step("unary")?;
        if depth > MAX_DEPTH {
            return Err(ExprError::new(self.tok_pos[self.idx], "expression too deeply nested"));
        }
        match self.peek() {
            Token::Minus => {
                self.next();
                let inner = self.parse_unary(depth + 1)?;
                Ok(crate::ast::Expr::Unary(UnaryOp::Neg, Box::new(inner)))
            }
            Token::Not => {
                self.next();
                let inner = self.parse_unary(depth + 1)?;
                Ok(crate::ast::Expr::Unary(UnaryOp::Not, Box::new(inner)))
            }
            _ => self.parse_primary(depth + 1),
        }
    }

    fn parse_primary(&mut self, depth: usize) -> Result<crate::ast::Expr, ExprError> {
        self.step("primary")?;
        let t = self.next();
        match t {
            Token::Num(s) => {
                let v = if s.contains('.') {
                    s.parse::<f64>()
                        .map(serde_json::Value::from)
                        .map_err(|_| ExprError::new(self.tok_pos[self.idx], "invalid number"))?
                } else if let Ok(i) = s.parse::<i64>() {
                    serde_json::Value::from(i)
                } else {
                    s.parse::<f64>()
                        .map(serde_json::Value::from)
                        .map_err(|_| ExprError::new(self.tok_pos[self.idx], "invalid number"))?
                };
                Ok(crate::ast::Expr::Literal(v))
            }
            Token::Str(s) => Ok(crate::ast::Expr::Literal(serde_json::Value::String(s))),
            Token::Path(p) => {
                let segs = crate::path::parse_path(&p)?;
                Ok(crate::ast::Expr::Path(segs))
            }
            Token::Ident(name) => {
                let lower = name.to_lowercase();
                match lower.as_str() {
                    "true" => Ok(crate::ast::Expr::Literal(serde_json::Value::Bool(true))),
                    "false" => Ok(crate::ast::Expr::Literal(serde_json::Value::Bool(false))),
                    "null" => Ok(crate::ast::Expr::Literal(serde_json::Value::Null)),
                    _ => {
                        if self.peek() == &Token::LParen {
                            self.next();
                            let mut args = Vec::new();
                            if self.peek() != &Token::RParen {
                                loop {
                                    args.push(self.parse_ternary(depth + 1)?);
                                    if self.peek() == &Token::Comma {
                                        self.next();
                                    } else {
                                        break;
                                    }
                                }
                            }
                            if self.peek() != &Token::RParen {
                                return Err(ExprError::new(self.tok_pos[self.idx], "expected ')'"));
                            }
                            self.next();
                            Ok(crate::ast::Expr::Call(name, args))
                        } else {
                            Err(ExprError::new(self.tok_pos[self.idx], format!("unknown identifier '{name}'")))
                        }
                    }
                }
            }
            Token::LParen => {
                let inner = self.parse_ternary(depth + 1)?;
                if self.peek() != &Token::RParen {
                    return Err(ExprError::new(self.tok_pos[self.idx], "expected ')'"));
                }
                self.next();
                Ok(inner)
            }
            other => Err(ExprError::new(self.tok_pos[self.idx], format!("unexpected token {other:?}"))),
        }
    }
}
