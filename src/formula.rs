//! Internal formula parser for RuleSpec formula strings.
//!
//! RuleSpec keeps formulas as concise strings. This module parses the small
//! expression language and lowers generated rule declarations into
//! [`crate::spec::ProgramSpec`].
//!
//! ```text
//! module      = (entity | definition | amend)*
//! definition  = (PATH | NAME) ":" metadata* temporal+
//! temporal    = "from" DATE [to DATE] ":" expr
//! expr        = match | cond | or_expr
//! ```
//!
//! Variables with no entity and a single literal value lower to parameters;
//! variables with an entity lower to derived outputs; richer mappings handle
//! match/cond/call/field access.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use std::collections::{BTreeMap, HashSet};
use std::str::FromStr;
use thiserror::Error;

use crate::spec::{
    ComparisonOpSpec, DTypeSpec, DerivedSemanticsSpec, DerivedSpec, DerivedVersionSpec,
    IndexedParameterSpec, JudgmentExprSpec, OverPeriodsKindSpec, ParameterVersionSpec, ProgramSpec,
    RelatedValueRefSpec, RelationSpec, ScalarExprSpec, ScalarValueSpec, UnitKindSpec, UnitSpec,
};

#[derive(Debug, Error)]
pub enum FormulaError {
    #[error("formula parse error at line {line}, col {col}: {message}")]
    Parse {
        line: usize,
        col: usize,
        message: String,
    },
    #[error("formula lower error: {0}")]
    Lower(String),
}

impl FormulaError {
    fn parse<S: Into<String>>(line: usize, col: usize, message: S) -> Self {
        FormulaError::Parse {
            line,
            col,
            message: message.into(),
        }
    }
    fn lower<S: Into<String>>(msg: S) -> Self {
        FormulaError::Lower(msg.into())
    }
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokType {
    Ident,
    Path,
    Int,
    Float,
    String,
    Date,
    // Keywords
    Entity,
    Amend,
    From,
    To,
    Match,
    If,
    Elif,
    Else,
    And,
    Or,
    Not,
    True,
    False,
    // Punctuation / operators
    Colon,
    Comma,
    Dot,
    Arrow,  // =>
    Fk,     // ->
    Assign, // =
    Le,
    Ge,
    Eq,
    Ne,
    Lt,
    Gt,
    Plus,
    Minus,
    Star,
    Slash,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Eof,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub ty: TokType,
    pub value: String,
    pub line: usize,
    pub col: usize,
}

fn keyword(s: &str) -> Option<TokType> {
    match s {
        "entity" => Some(TokType::Entity),
        "amend" => Some(TokType::Amend),
        "from" => Some(TokType::From),
        "to" => Some(TokType::To),
        "match" => Some(TokType::Match),
        "if" => Some(TokType::If),
        "elif" => Some(TokType::Elif),
        "else" => Some(TokType::Else),
        "and" => Some(TokType::And),
        "or" => Some(TokType::Or),
        "not" => Some(TokType::Not),
        "true" | "True" => Some(TokType::True),
        "false" | "False" => Some(TokType::False),
        _ => None,
    }
}

pub struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
    line: usize,
    col: usize,
    tokens: Vec<Token>,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Self {
            src: src.as_bytes(),
            pos: 0,
            line: 1,
            col: 1,
            tokens: Vec::new(),
        }
    }

    fn advance(&mut self, n: usize) {
        for _ in 0..n {
            if self.pos >= self.src.len() {
                break;
            }
            let c = self.src[self.pos];
            self.pos += 1;
            if c == b'\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
        }
    }

    fn push(&mut self, ty: TokType, value: String, line: usize, col: usize) {
        self.tokens.push(Token {
            ty,
            value,
            line,
            col,
        });
    }

    pub fn tokenise(mut self) -> Result<Vec<Token>, FormulaError> {
        while self.pos < self.src.len() {
            // Triple-quoted block (statute docstring). Skip without emitting.
            if self.pos + 3 <= self.src.len() && &self.src[self.pos..self.pos + 3] == b"\"\"\"" {
                let start = self.pos + 3;
                if let Some(rel) = find_bytes(&self.src[start..], b"\"\"\"") {
                    self.advance(rel + 6);
                } else {
                    self.advance(self.src.len() - self.pos);
                }
                continue;
            }

            // Comment
            if self.src[self.pos] == b'#' {
                while self.pos < self.src.len() && self.src[self.pos] != b'\n' {
                    self.advance(1);
                }
                continue;
            }

            // Whitespace
            if self.src[self.pos].is_ascii_whitespace() {
                self.advance(1);
                continue;
            }

            let line = self.line;
            let col = self.col;
            let c = self.src[self.pos];

            // Date: YYYY-MM-DD
            if c.is_ascii_digit() && self.pos + 10 <= self.src.len() {
                let slice = &self.src[self.pos..self.pos + 10];
                if slice[4] == b'-'
                    && slice[7] == b'-'
                    && slice
                        .iter()
                        .enumerate()
                        .all(|(i, &b)| matches!(i, 4 | 7) || b.is_ascii_digit())
                {
                    let s = std::str::from_utf8(slice).unwrap().to_string();
                    self.advance(10);
                    self.push(TokType::Date, s, line, col);
                    continue;
                }
            }

            // Number (int or float). Allows underscores.
            if c.is_ascii_digit() {
                let mut end = self.pos;
                while end < self.src.len()
                    && (self.src[end].is_ascii_digit() || self.src[end] == b'_')
                {
                    end += 1;
                }
                let mut is_float = false;
                if end < self.src.len() && self.src[end] == b'.' {
                    // Lookahead: next must be digit
                    if end + 1 < self.src.len() && self.src[end + 1].is_ascii_digit() {
                        is_float = true;
                        end += 1;
                        while end < self.src.len()
                            && (self.src[end].is_ascii_digit() || self.src[end] == b'_')
                        {
                            end += 1;
                        }
                    }
                }
                let s = std::str::from_utf8(&self.src[self.pos..end])
                    .unwrap()
                    .to_string();
                let n = end - self.pos;
                self.advance(n);
                self.push(
                    if is_float {
                        TokType::Float
                    } else {
                        TokType::Int
                    },
                    s,
                    line,
                    col,
                );
                continue;
            }

            // String literal
            if c == b'"' || c == b'\'' {
                let quote = c;
                self.advance(1);
                let mut value = String::new();
                let mut segment_start = self.pos;
                while self.pos < self.src.len() {
                    let current = self.src[self.pos];
                    if current == quote {
                        if segment_start < self.pos {
                            value.push_str(
                                std::str::from_utf8(&self.src[segment_start..self.pos]).unwrap(),
                            );
                        }
                        self.advance(1);
                        self.push(TokType::String, value, line, col);
                        break;
                    }
                    if current == b'\\' {
                        if segment_start < self.pos {
                            value.push_str(
                                std::str::from_utf8(&self.src[segment_start..self.pos]).unwrap(),
                            );
                        }
                        if self.pos + 1 >= self.src.len() {
                            return Err(FormulaError::parse(line, col, "unterminated string"));
                        }
                        let escaped = self.src[self.pos + 1];
                        value.push(match escaped {
                            b'\\' => '\\',
                            b'"' => '"',
                            b'\'' => '\'',
                            b'n' => '\n',
                            b'r' => '\r',
                            b't' => '\t',
                            other => other as char,
                        });
                        self.advance(2);
                        segment_start = self.pos;
                        continue;
                    }
                    self.advance(1);
                }
                if self.tokens.last().is_some_and(|token| {
                    token.ty == TokType::String && token.line == line && token.col == col
                }) {
                    continue;
                }
                return Err(FormulaError::parse(line, col, "unterminated string"));
            }

            // Identifier / path / keyword
            if c.is_ascii_alphabetic() || c == b'_' {
                let mut end = self.pos;
                while end < self.src.len()
                    && (self.src[end].is_ascii_alphanumeric() || self.src[end] == b'_')
                {
                    end += 1;
                }
                // Path: IDENT (/ IDENT)+
                let mut is_path = false;
                let mut path_end = end;
                while path_end < self.src.len() && self.src[path_end] == b'/' {
                    let next_start = path_end + 1;
                    if next_start < self.src.len()
                        && (self.src[next_start].is_ascii_alphabetic()
                            || self.src[next_start] == b'_')
                    {
                        is_path = true;
                        let mut q = next_start + 1;
                        while q < self.src.len()
                            && (self.src[q].is_ascii_alphanumeric() || self.src[q] == b'_')
                        {
                            q += 1;
                        }
                        path_end = q;
                    } else {
                        break;
                    }
                }
                let final_end = if is_path { path_end } else { end };
                let s = std::str::from_utf8(&self.src[self.pos..final_end])
                    .unwrap()
                    .to_string();
                let n = final_end - self.pos;
                self.advance(n);
                if !is_path {
                    if let Some(kw) = keyword(&s) {
                        self.push(kw, s, line, col);
                    } else {
                        self.push(TokType::Ident, s, line, col);
                    }
                } else {
                    self.push(TokType::Path, s, line, col);
                }
                continue;
            }

            // Multi-char operators
            if self.pos + 2 <= self.src.len() {
                let pair = &self.src[self.pos..self.pos + 2];
                let ty = match pair {
                    b"=>" => Some(TokType::Arrow),
                    b"<=" => Some(TokType::Le),
                    b">=" => Some(TokType::Ge),
                    b"==" => Some(TokType::Eq),
                    b"!=" => Some(TokType::Ne),
                    b"->" => Some(TokType::Fk),
                    _ => None,
                };
                if let Some(ty) = ty {
                    let s = std::str::from_utf8(pair).unwrap().to_string();
                    self.advance(2);
                    self.push(ty, s, line, col);
                    continue;
                }
            }

            // Single-char operators
            let (ty, len) = match c {
                b':' => (TokType::Colon, 1),
                b',' => (TokType::Comma, 1),
                b'.' => (TokType::Dot, 1),
                b'=' => (TokType::Assign, 1),
                b'+' => (TokType::Plus, 1),
                b'-' => (TokType::Minus, 1),
                b'*' => (TokType::Star, 1),
                b'/' => (TokType::Slash, 1),
                b'<' => (TokType::Lt, 1),
                b'>' => (TokType::Gt, 1),
                b'(' => (TokType::LParen, 1),
                b')' => (TokType::RParen, 1),
                b'[' => (TokType::LBracket, 1),
                b']' => (TokType::RBracket, 1),
                _ => {
                    return Err(FormulaError::parse(
                        line,
                        col,
                        format!("unexpected char {:?}", c as char),
                    ));
                }
            };
            let s = (c as char).to_string();
            self.advance(len);
            self.push(ty, s, line, col);
        }

        self.push(TokType::Eof, String::new(), self.line, self.col);
        Ok(self.tokens)
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

// ---------------------------------------------------------------------------
// AST
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum Expr {
    LitInt(i64),
    LitFloat(Decimal),
    LitStr(String),
    LitBool(bool),
    Var(String),
    BinOp {
        op: BinOpKind,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    UnaryOp {
        op: UnaryOpKind,
        operand: Box<Expr>,
    },
    Call {
        func: String,
        args: Vec<Expr>,
    },
    FieldAccess {
        obj: Box<Expr>,
        field: String,
    },
    Index {
        target: Box<Expr>,
        index: Box<Expr>,
    },
    Cond {
        condition: Box<Expr>,
        then_expr: Box<Expr>,
        else_expr: Box<Expr>,
    },
    Match {
        subject: Box<Expr>,
        cases: Vec<(Expr, Expr)>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BinOpKind {
    Add,
    Sub,
    Mul,
    Div,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
    And,
    Or,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnaryOpKind {
    Neg,
    Not,
}

#[derive(Clone, Debug)]
pub struct TemporalValue {
    pub start: NaiveDate,
    pub expr: Expr,
}

#[derive(Clone, Debug, Default)]
pub struct VariableDecl {
    pub path: String,
    pub entity: Option<String>,
    pub dtype: Option<String>,
    pub period: Option<String>,
    pub unit: Option<String>,
    pub source: Option<String>,
    pub source_url: Option<String>,
    pub label: Option<String>,
    pub description: Option<String>,
    pub default: Option<String>,
    pub indexed_by: Option<String>,
    pub values: Vec<TemporalValue>,
}

#[derive(Clone, Debug, Default)]
pub struct Module {
    pub variables: Vec<VariableDecl>,
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

const METADATA_FIELDS: &[&str] = &[
    "source",
    "source_url",
    "label",
    "description",
    "unit",
    "dtype",
    "period",
    "default",
    "indexed_by",
    "status",
];

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self, offset: usize) -> &Token {
        let idx = (self.pos + offset).min(self.tokens.len() - 1);
        &self.tokens[idx]
    }

    fn at(&self, tys: &[TokType]) -> bool {
        tys.iter().any(|t| self.peek(0).ty == *t)
    }

    fn consume(&mut self, ty: TokType) -> Result<Token, FormulaError> {
        let tok = self.peek(0).clone();
        if tok.ty != ty {
            return Err(FormulaError::parse(
                tok.line,
                tok.col,
                format!("expected {:?}, got {:?} ({})", ty, tok.ty, tok.value),
            ));
        }
        self.pos += 1;
        Ok(tok)
    }

    fn match_one(&mut self, tys: &[TokType]) -> Option<Token> {
        if self.at(tys) {
            let tok = self.peek(0).clone();
            self.pos += 1;
            Some(tok)
        } else {
            None
        }
    }

    pub fn parse_module(&mut self) -> Result<Module, FormulaError> {
        let mut module = Module::default();
        while self.peek(0).ty != TokType::Eof {
            if self.at(&[TokType::Entity]) {
                // Skip entity declarations entirely — engine infers entities from data.
                self.skip_entity()?;
                continue;
            }
            if self.at(&[TokType::Amend]) {
                // Amendments not used by our test corpus yet; skip.
                self.skip_amend()?;
                continue;
            }
            if self.at(&[TokType::Ident, TokType::Path]) && self.peek(1).ty == TokType::Colon {
                // Skip bare metadata lines like `status: boilerplate` with no body
                if self.peek(0).ty == TokType::Ident
                    && self.peek(0).value == "status"
                    && matches!(self.peek(2).ty, TokType::Ident | TokType::String)
                    && self.peek(3).ty != TokType::Colon
                {
                    self.pos += 3;
                    continue;
                }
                module.variables.push(self.parse_variable()?);
                continue;
            }
            let tok = self.peek(0).clone();
            return Err(FormulaError::parse(
                tok.line,
                tok.col,
                format!("unexpected token {:?} ({})", tok.ty, tok.value),
            ));
        }
        Ok(module)
    }

    fn skip_entity(&mut self) -> Result<(), FormulaError> {
        self.consume(TokType::Entity)?;
        self.consume(TokType::Ident)?;
        self.consume(TokType::Colon)?;
        // Skip fields while we see `ident: ...`
        while self.peek(0).ty == TokType::Ident && self.peek(1).ty == TokType::Colon {
            // Lookahead: does this look like a variable declaration rather
            // than an entity field? A variable decl has a metadata line or
            // a `from` block after. Entity fields have a single type token.
            // Simplest heuristic: if the token after the colon is a
            // single IDENT followed by another `ident:` pair (next field)
            // or EOF/ENTITY/AMEND, treat as a field.
            let after_colon = self.peek(2).ty.clone();
            if !matches!(
                after_colon,
                TokType::Ident | TokType::LBracket | TokType::Fk
            ) {
                break;
            }
            self.consume(TokType::Ident)?; // field name
            self.consume(TokType::Colon)?;
            if self.at(&[TokType::Fk]) {
                self.consume(TokType::Fk)?;
                self.consume(TokType::Ident)?;
            } else if self.at(&[TokType::LBracket]) {
                self.consume(TokType::LBracket)?;
                self.consume(TokType::Ident)?;
                self.consume(TokType::RBracket)?;
            } else {
                self.consume(TokType::Ident)?; // type name
            }
        }
        Ok(())
    }

    fn skip_amend(&mut self) -> Result<(), FormulaError> {
        self.consume(TokType::Amend)?;
        // Target path
        if self.at(&[TokType::Path]) {
            self.consume(TokType::Path)?;
        } else {
            self.consume(TokType::Ident)?;
        }
        self.consume(TokType::Colon)?;
        // Skip temporal values
        while self.at(&[TokType::From]) {
            self.consume(TokType::From)?;
            self.consume(TokType::Date)?;
            if self.at(&[TokType::To]) {
                self.consume(TokType::To)?;
                self.consume(TokType::Date)?;
            }
            self.consume(TokType::Colon)?;
            self.parse_expr()?;
        }
        Ok(())
    }

    fn parse_variable(&mut self) -> Result<VariableDecl, FormulaError> {
        let path = self.parse_path()?;
        self.consume(TokType::Colon)?;
        let mut decl = VariableDecl {
            path,
            ..Default::default()
        };
        // Metadata + optional entity, any order
        loop {
            if self.at(&[TokType::Entity]) {
                self.consume(TokType::Entity)?;
                self.consume(TokType::Colon)?;
                let t = self.consume(TokType::Ident)?;
                decl.entity = Some(t.value);
                continue;
            }
            if self.peek(0).ty == TokType::Ident
                && self.peek(1).ty == TokType::Colon
                && METADATA_FIELDS.contains(&self.peek(0).value.as_str())
            {
                let field = self.consume(TokType::Ident)?.value;
                self.consume(TokType::Colon)?;
                let tok = self.peek(0).clone();
                let v = match tok.ty {
                    TokType::String => self.consume(TokType::String)?.value,
                    TokType::Ident | TokType::Int | TokType::Float => {
                        let mut val = self.consume(tok.ty.clone())?.value;
                        // Skip type-suffix brackets e.g. `Enum[Status]`
                        if self.at(&[TokType::LBracket]) {
                            self.consume(TokType::LBracket)?;
                            let mut depth = 1;
                            while depth > 0 && self.peek(0).ty != TokType::Eof {
                                match self.peek(0).ty {
                                    TokType::LBracket => depth += 1,
                                    TokType::RBracket => depth -= 1,
                                    _ => {}
                                }
                                self.pos += 1;
                            }
                        }
                        // Handle unit with path-like value (e.g. `unit: GBP/week`)
                        // already a Path token; handled above as Ident branch won't fire
                        let _ = &mut val;
                        val
                    }
                    TokType::Path => self.consume(TokType::Path)?.value,
                    TokType::Minus => {
                        self.consume(TokType::Minus)?;
                        let t = self.peek(0).ty.clone();
                        let v = self.consume(t)?.value;
                        format!("-{v}")
                    }
                    TokType::True => {
                        self.consume(TokType::True)?;
                        "true".to_string()
                    }
                    TokType::False => {
                        self.consume(TokType::False)?;
                        "false".to_string()
                    }
                    _ => {
                        return Err(FormulaError::parse(
                            tok.line,
                            tok.col,
                            format!("unexpected metadata value {:?}", tok.ty),
                        ));
                    }
                };
                match field.as_str() {
                    "source" => decl.source = Some(v),
                    "source_url" => decl.source_url = Some(v),
                    "label" => decl.label = Some(v),
                    "description" => decl.description = Some(v),
                    "unit" => decl.unit = Some(v),
                    "dtype" => decl.dtype = Some(v),
                    "period" => decl.period = Some(v),
                    "default" => decl.default = Some(v),
                    "indexed_by" => decl.indexed_by = Some(v),
                    "status" => {}
                    _ => {}
                }
                continue;
            }
            break;
        }
        // Temporal values: at least one `from DATE [to DATE]: expr`
        while self.at(&[TokType::From]) {
            self.consume(TokType::From)?;
            let start_tok = self.consume(TokType::Date)?;
            let start = NaiveDate::parse_from_str(&start_tok.value, "%Y-%m-%d")
                .map_err(|e| FormulaError::parse(start_tok.line, start_tok.col, e.to_string()))?;
            if self.at(&[TokType::To]) {
                self.consume(TokType::To)?;
                let t = self.consume(TokType::Date)?;
                NaiveDate::parse_from_str(&t.value, "%Y-%m-%d")
                    .map_err(|e| FormulaError::parse(t.line, t.col, e.to_string()))?;
            }
            self.consume(TokType::Colon)?;
            let expr = self.parse_expr()?;
            decl.values.push(TemporalValue { start, expr });
        }
        Ok(decl)
    }

    fn parse_path(&mut self) -> Result<String, FormulaError> {
        if self.at(&[TokType::Path]) {
            Ok(self.consume(TokType::Path)?.value)
        } else {
            Ok(self.consume(TokType::Ident)?.value)
        }
    }

    fn parse_expr(&mut self) -> Result<Expr, FormulaError> {
        // Let-bindings: IDENT ASSIGN value body
        if self.peek(0).ty == TokType::Ident && self.peek(1).ty == TokType::Assign {
            // Let-bindings aren't lowered by our engine yet. Inline the value
            // at the use site manually; until then, this is an error if hit.
            let tok = self.peek(0).clone();
            return Err(FormulaError::parse(
                tok.line,
                tok.col,
                "let-bindings are not supported in RuleSpec formulas yet".to_string(),
            ));
        }
        if self.at(&[TokType::Match]) {
            return self.parse_match();
        }
        if self.at(&[TokType::If]) {
            return self.parse_cond();
        }
        self.parse_or()
    }

    fn parse_match(&mut self) -> Result<Expr, FormulaError> {
        self.consume(TokType::Match)?;
        let subject = self.parse_or()?;
        self.consume(TokType::Colon)?;
        let mut cases = Vec::new();
        while matches!(
            self.peek(0).ty,
            TokType::Int
                | TokType::Float
                | TokType::String
                | TokType::True
                | TokType::False
                | TokType::Ident
        ) && self.peek(1).ty == TokType::Arrow
        {
            let pat = self.parse_primary()?;
            self.consume(TokType::Arrow)?;
            let res = self.parse_expr()?;
            cases.push((pat, res));
        }
        Ok(Expr::Match {
            subject: Box::new(subject),
            cases,
        })
    }

    fn parse_cond(&mut self) -> Result<Expr, FormulaError> {
        self.consume(TokType::If)?;
        let cond = self.parse_or()?;
        self.consume(TokType::Colon)?;
        let then_expr = self.parse_expr()?;
        let else_expr = if self.match_one(&[TokType::Elif]).is_some() {
            let elif_cond = self.parse_or()?;
            self.consume(TokType::Colon)?;
            let elif_then = self.parse_expr()?;
            self.parse_elif_chain(elif_cond, elif_then)?
        } else {
            self.consume(TokType::Else)?;
            self.consume(TokType::Colon)?;
            self.parse_expr()?
        };
        Ok(Expr::Cond {
            condition: Box::new(cond),
            then_expr: Box::new(then_expr),
            else_expr: Box::new(else_expr),
        })
    }

    fn parse_elif_chain(&mut self, cond: Expr, then_expr: Expr) -> Result<Expr, FormulaError> {
        let else_expr = if self.match_one(&[TokType::Elif]).is_some() {
            let next_cond = self.parse_or()?;
            self.consume(TokType::Colon)?;
            let next_then = self.parse_expr()?;
            self.parse_elif_chain(next_cond, next_then)?
        } else {
            self.consume(TokType::Else)?;
            self.consume(TokType::Colon)?;
            self.parse_expr()?
        };
        Ok(Expr::Cond {
            condition: Box::new(cond),
            then_expr: Box::new(then_expr),
            else_expr: Box::new(else_expr),
        })
    }

    fn parse_or(&mut self) -> Result<Expr, FormulaError> {
        let mut left = self.parse_and()?;
        while self.match_one(&[TokType::Or]).is_some() {
            let right = self.parse_and()?;
            left = Expr::BinOp {
                op: BinOpKind::Or,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, FormulaError> {
        let mut left = self.parse_cmp()?;
        while self.match_one(&[TokType::And]).is_some() {
            let right = self.parse_cmp()?;
            left = Expr::BinOp {
                op: BinOpKind::And,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_cmp(&mut self) -> Result<Expr, FormulaError> {
        let left = self.parse_add()?;
        let op = match self.peek(0).ty {
            TokType::Lt => Some(BinOpKind::Lt),
            TokType::Gt => Some(BinOpKind::Gt),
            TokType::Le => Some(BinOpKind::Le),
            TokType::Ge => Some(BinOpKind::Ge),
            TokType::Eq => Some(BinOpKind::Eq),
            TokType::Ne => Some(BinOpKind::Ne),
            _ => None,
        };
        if let Some(op) = op {
            self.pos += 1;
            let right = self.parse_add()?;
            return Ok(Expr::BinOp {
                op,
                left: Box::new(left),
                right: Box::new(right),
            });
        }
        Ok(left)
    }

    fn parse_add(&mut self) -> Result<Expr, FormulaError> {
        let mut left = self.parse_mul()?;
        loop {
            let op = match self.peek(0).ty {
                TokType::Plus => BinOpKind::Add,
                TokType::Minus => BinOpKind::Sub,
                _ => break,
            };
            self.pos += 1;
            let right = self.parse_mul()?;
            left = Expr::BinOp {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_mul(&mut self) -> Result<Expr, FormulaError> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek(0).ty {
                TokType::Star => BinOpKind::Mul,
                TokType::Slash => BinOpKind::Div,
                _ => break,
            };
            self.pos += 1;
            let right = self.parse_unary()?;
            left = Expr::BinOp {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, FormulaError> {
        if self.match_one(&[TokType::Minus]).is_some() {
            let operand = self.parse_unary()?;
            return Ok(Expr::UnaryOp {
                op: UnaryOpKind::Neg,
                operand: Box::new(operand),
            });
        }
        if self.match_one(&[TokType::Not]).is_some() {
            let operand = self.parse_unary()?;
            return Ok(Expr::UnaryOp {
                op: UnaryOpKind::Not,
                operand: Box::new(operand),
            });
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Result<Expr, FormulaError> {
        let mut expr = self.parse_primary()?;
        loop {
            if self.at(&[TokType::LParen]) {
                // Call — expr must be a Var
                let func = match &expr {
                    Expr::Var(name) => name.clone(),
                    _ => {
                        let tok = self.peek(0).clone();
                        return Err(FormulaError::parse(
                            tok.line,
                            tok.col,
                            "can only call named functions".to_string(),
                        ));
                    }
                };
                self.consume(TokType::LParen)?;
                let mut args = Vec::new();
                if !self.at(&[TokType::RParen]) {
                    args.push(self.parse_expr()?);
                    while self.match_one(&[TokType::Comma]).is_some() {
                        args.push(self.parse_expr()?);
                    }
                }
                self.consume(TokType::RParen)?;
                expr = Expr::Call { func, args };
            } else if self.at(&[TokType::Dot]) {
                self.consume(TokType::Dot)?;
                let field = self.consume(TokType::Ident)?.value;
                expr = Expr::FieldAccess {
                    obj: Box::new(expr),
                    field,
                };
            } else if self.at(&[TokType::LBracket]) {
                self.consume(TokType::LBracket)?;
                let index = self.parse_expr()?;
                self.consume(TokType::RBracket)?;
                expr = Expr::Index {
                    target: Box::new(expr),
                    index: Box::new(index),
                };
            } else {
                break;
            }
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr, FormulaError> {
        let tok = self.peek(0).clone();
        match tok.ty {
            TokType::Int => {
                self.pos += 1;
                let v: i64 =
                    tok.value
                        .replace('_', "")
                        .parse()
                        .map_err(|e: std::num::ParseIntError| {
                            FormulaError::parse(tok.line, tok.col, e.to_string())
                        })?;
                Ok(Expr::LitInt(v))
            }
            TokType::Float => {
                self.pos += 1;
                let v = Decimal::from_str(&tok.value.replace('_', ""))
                    .map_err(|e| FormulaError::parse(tok.line, tok.col, e.to_string()))?;
                Ok(Expr::LitFloat(v))
            }
            TokType::String => {
                self.pos += 1;
                Ok(Expr::LitStr(tok.value))
            }
            TokType::True => {
                self.pos += 1;
                Ok(Expr::LitBool(true))
            }
            TokType::False => {
                self.pos += 1;
                Ok(Expr::LitBool(false))
            }
            TokType::Ident | TokType::Path => {
                self.pos += 1;
                Ok(Expr::Var(tok.value))
            }
            TokType::LParen => {
                self.consume(TokType::LParen)?;
                let e = self.parse_expr()?;
                self.consume(TokType::RParen)?;
                Ok(e)
            }
            _ => Err(FormulaError::parse(
                tok.line,
                tok.col,
                format!(
                    "unexpected token in expression: {:?} ({})",
                    tok.ty, tok.value
                ),
            )),
        }
    }
}

pub fn parse_source(src: &str) -> Result<Module, FormulaError> {
    let tokens = Lexer::new(src).tokenise()?;
    let mut parser = Parser::new(tokens);
    parser.parse_module()
}

// ---------------------------------------------------------------------------
// Lowering to engine's Program model
// ---------------------------------------------------------------------------

/// Lower a parsed RuleSpec formula declaration module into `ProgramSpec`.
///
/// Convention for now:
/// - A `VariableDecl` with no `entity` whose single temporal value is a
///   literal lowers to a scalar `Parameter` keyed at index 0 (or at integer
///   keys if the literal is an integer-keyed table once indexed_by support
///   lands).
/// - A `VariableDecl` with an `entity` lowers to a `Derived` output. Versioned
///   derived formulas lower to dated derived versions selected by query period.
/// - Non-literal scalar variables (with no entity but a computed expression)
///   lower to derived outputs attached to a synthetic `Scalar` entity so
///   they can be referenced from entity-scoped derived values.
pub fn lower_module(module: &Module) -> Result<ProgramSpec, FormulaError> {
    let mut program = ProgramSpec::default();

    // Helpful unit defaults so RuleSpec modules don't have to re-declare common
    // units inline. RuleSpec modules may override / add their own units by
    // declaring them, but RuleSpec formula declarations only carry unit names on
    // a variable declaration — we seed the spec with the commonly used ones.
    for (name, kind) in [
        ("GBP", UnitKindSpec::Currency { minor_units: 2 }),
        ("USD", UnitKindSpec::Currency { minor_units: 2 }),
        ("EUR", UnitKindSpec::Currency { minor_units: 2 }),
        ("GHS", UnitKindSpec::Currency { minor_units: 2 }),
        // Ghana pesewas: the Energy Sector Levies Act, 2025 schedule states
        // its per-litre and per-kilogramme rates in GHp, so modules ground
        // those amounts in the schedule's own unit (100 GHp = 1 GHS).
        ("GHp", UnitKindSpec::Currency { minor_units: 0 }),
        ("NGN", UnitKindSpec::Currency { minor_units: 2 }),
        // Uganda Shilling: ISO 4217 lists no minor unit in circulation
        // (exponent 0); Ugandan statutes state amounts in whole shillings.
        ("UGX", UnitKindSpec::Currency { minor_units: 0 }),
        // Zambian Kwacha (rebased 2013): ISO 4217 exponent 2 (100 ngwee
        // = 1 kwacha); Zambian statutes state amounts in kwacha and ngwee.
        ("ZMW", UnitKindSpec::Currency { minor_units: 2 }),
        // Ethiopian Birr: ISO 4217 exponent 2 (100 santim = 1 birr);
        // Ethiopian proclamations state amounts in birr.
        ("ETB", UnitKindSpec::Currency { minor_units: 2 }),
        ("count", UnitKindSpec::Count),
        ("person", UnitKindSpec::Count),
        ("ratio", UnitKindSpec::Ratio),
        (
            "status",
            UnitKindSpec::Custom {
                label: "judgment".to_string(),
            },
        ),
    ] {
        program.units.push(UnitSpec {
            name: name.to_string(),
            kind,
        });
    }

    // Collect names so other variables can reference them correctly. No-
    // entity literal-only vars are scalar parameters; vars with an entity
    // are derived outputs; no-entity computed vars are attached to a
    // synthetic `Scalar` entity.
    let mut scalar_names: HashSet<String> = HashSet::new();
    let mut derived_names: HashSet<String> = HashSet::new();
    for v in &module.variables {
        if v.entity.is_some() {
            derived_names.insert(v.path.clone());
        } else if v.values.iter().all(|t| is_literal_expr(&t.expr)) {
            scalar_names.insert(v.path.clone());
        } else {
            derived_names.insert(v.path.clone());
        }
    }

    // First pass: scalar parameters.
    for v in &module.variables {
        if v.entity.is_some() {
            continue;
        }
        if !v.values.iter().all(|t| is_literal_expr(&t.expr)) {
            continue;
        }
        let mut versions: Vec<ParameterVersionSpec> = Vec::new();
        for t in &v.values {
            let mut values: BTreeMap<i64, ScalarValueSpec> = BTreeMap::new();
            values.insert(0, literal_to_scalar_value(&t.expr)?);
            versions.push(ParameterVersionSpec {
                effective_from: t.start,
                values,
            });
        }
        program.parameters.push(IndexedParameterSpec {
            id: None,
            name: v.path.clone(),
            unit: v.unit.clone(),
            indexed_by: v.indexed_by.clone(),
            source: v.source.clone(),
            source_url: v.source_url.clone(),
            // The formula-source layer doesn't carry the origin module's
            // citation path; it is reattached by name in `apply_rule_ids`.
            corpus_citation_path: None,
            versions,
        });
    }

    // Second pass: derived outputs. Single-version derived outputs use the
    // historical semantics field; multi-version outputs also carry dated
    // semantics for runtime period selection.
    let ctx = LowerCtx {
        scalars: scalar_names,
        derived: derived_names,
        relation_predicates: HashSet::new(),
        relations: std::cell::RefCell::new(HashSet::new()),
    };
    for v in &module.variables {
        if v.entity.is_none() && v.values.iter().all(|t| is_literal_expr(&t.expr)) {
            continue;
        }
        let dtype = parse_dtype(v.dtype.as_deref());
        let entity = v
            .entity
            .clone()
            .unwrap_or_else(|| crate::model::SCALAR_ENTITY.to_string());
        let last = v.values.last().ok_or_else(|| {
            FormulaError::lower(format!("variable `{}` has no temporal values", v.path))
        })?;
        let lower_semantics =
            |expr: &Expr, dtype: &DTypeSpec| -> Result<DerivedSemanticsSpec, FormulaError> {
                match dtype {
                    DTypeSpec::Judgment => Ok(DerivedSemanticsSpec::Judgment {
                        expr: lower_to_judgment(expr, &ctx)?,
                    }),
                    DTypeSpec::Decimal => {
                        let mut expr = lower_to_scalar(expr, &ctx)?;
                        promote_ints_to_decimal(&mut expr);
                        Ok(DerivedSemanticsSpec::Scalar { expr })
                    }
                    _ => Ok(DerivedSemanticsSpec::Scalar {
                        expr: lower_to_scalar(expr, &ctx)?,
                    }),
                }
            };
        let semantics = lower_semantics(&last.expr, &dtype)?;
        let versions = if v.values.len() > 1 {
            v.values
                .iter()
                .map(|value| {
                    Ok(DerivedVersionSpec {
                        effective_from: value.start,
                        semantics: lower_semantics(&value.expr, &dtype)?,
                    })
                })
                .collect::<Result<Vec<DerivedVersionSpec>, FormulaError>>()?
        } else {
            Vec::new()
        };
        program.derived.push(DerivedSpec {
            id: None,
            name: v.path.clone(),
            entity,
            dtype,
            unit: v.unit.clone(),
            period: v.period.clone(),
            // The formula-source layer doesn't carry rounding; the RuleSpec
            // `rounding:` mode is reattached by name in `apply_rule_ids`.
            rounding: None,
            source: v.source.clone(),
            source_url: v.source_url.clone(),
            // As with rounding, the origin module's citation path is
            // reattached by name in `apply_rule_ids`.
            corpus_citation_path: None,
            semantics,
            versions,
        });
    }

    // Emit relation declarations inferred from aggregation usage.
    let mut relation_names: Vec<String> = ctx.relations.borrow().iter().cloned().collect();
    relation_names.sort();
    for name in relation_names {
        program.relations.push(RelationSpec {
            name,
            arity: 2,
            derivation: None,
        });
    }

    Ok(program)
}

/// Slot convention for two-slot relations referenced from aggregations.
/// Names of the form `X_of_Y` (or `X_of_a_Y` …) read as "X belongs to Y",
/// so slot 0 = X (related item being iterated) and slot 1 = Y (the
/// enclosing / current entity). Any other naming is assumed to put the
/// enclosing entity at slot 0 and the related item at slot 1.
fn infer_slots(_relation: &str) -> (usize, usize) {
    // Default slot convention: current entity is slot 1, related item is
    // slot 0. Matches how every existing YAML RuleSpec module declares its
    // relations (adult_of_benefit_unit, child_of_claim, cb_receipt,
    // liable_person, associate_of, council_notice_of_tenancy, …). A
    // RuleSpec module that needs the opposite orientation should rename the
    // relation to put the enclosing entity last (e.g.
    // `disposal_of_applicant` rather than `applicant_disposal`).
    (1, 0)
}

pub(crate) fn infer_relation_slots_for_rulespec(relation: &str) -> (usize, usize) {
    infer_slots(relation)
}

fn is_literal_expr(e: &Expr) -> bool {
    matches!(
        e,
        Expr::LitInt(_) | Expr::LitFloat(_) | Expr::LitBool(_) | Expr::LitStr(_)
    )
}

fn literal_to_scalar_value(e: &Expr) -> Result<ScalarValueSpec, FormulaError> {
    match e {
        Expr::LitInt(i) => Ok(ScalarValueSpec::Integer { value: *i }),
        Expr::LitFloat(d) => Ok(ScalarValueSpec::Decimal {
            value: d.normalize().to_string(),
        }),
        Expr::LitBool(b) => Ok(ScalarValueSpec::Bool { value: *b }),
        Expr::LitStr(s) => Ok(ScalarValueSpec::Text { value: s.clone() }),
        _ => Err(FormulaError::lower(
            "expected literal expression".to_string(),
        )),
    }
}

fn parse_dtype(s: Option<&str>) -> DTypeSpec {
    match s {
        Some("Judgment") | Some("judgment") => DTypeSpec::Judgment,
        Some("Boolean") | Some("Bool") | Some("boolean") | Some("bool") => DTypeSpec::Bool,
        Some("Integer") | Some("integer") | Some("int") => DTypeSpec::Integer,
        Some("Money") | Some("money") | Some("Rate") | Some("rate") | Some("Decimal")
        | Some("decimal") | Some("float") => DTypeSpec::Decimal,
        Some("Text") | Some("text") | Some("String") | Some("string") => DTypeSpec::Text,
        Some("Date") | Some("date") => DTypeSpec::Date,
        _ => DTypeSpec::Decimal,
    }
}

struct LowerCtx {
    scalars: HashSet<String>,
    derived: HashSet<String>,
    relation_predicates: HashSet<String>,
    // Relations discovered while lowering expressions (len / sum /
    // count_where / sum_where) so we can emit matching RelationSpec
    // declarations without requiring an explicit entity-declaration
    // block in the source.
    relations: std::cell::RefCell<HashSet<String>>,
}

fn lit_int_spec(i: i64) -> ScalarExprSpec {
    ScalarExprSpec::Literal {
        value: ScalarValueSpec::Integer { value: i },
    }
}

fn lit_bool_spec(b: bool) -> ScalarExprSpec {
    ScalarExprSpec::Literal {
        value: ScalarValueSpec::Bool { value: b },
    }
}

fn lower_to_scalar(e: &Expr, ctx: &LowerCtx) -> Result<ScalarExprSpec, FormulaError> {
    Ok(match e {
        Expr::LitInt(i) => lit_int_spec(*i),
        Expr::LitFloat(d) => ScalarExprSpec::Literal {
            value: ScalarValueSpec::Decimal {
                value: d.normalize().to_string(),
            },
        },
        Expr::LitBool(b) => lit_bool_spec(*b),
        Expr::LitStr(s) => ScalarExprSpec::Literal {
            value: ScalarValueSpec::Text { value: s.clone() },
        },
        Expr::Var(name) => match name.as_str() {
            "period_start" => ScalarExprSpec::PeriodStart,
            "period_end" => ScalarExprSpec::PeriodEnd,
            _ => {
                if ctx.scalars.contains(name) {
                    ScalarExprSpec::ParameterLookup {
                        parameter: name.clone(),
                        index: Box::new(lit_int_spec(0)),
                    }
                } else if ctx.derived.contains(name) {
                    ScalarExprSpec::Derived { name: name.clone() }
                } else {
                    ScalarExprSpec::Input { name: name.clone() }
                }
            }
        },
        Expr::Index { target, index } => {
            let Expr::Var(parameter) = target.as_ref() else {
                return Err(FormulaError::lower(
                    "parameter lookup target must be a named parameter".to_string(),
                ));
            };
            if !ctx.scalars.contains(parameter) {
                return Err(FormulaError::lower(format!(
                    "`{parameter}` is not a parameter table"
                )));
            }
            ScalarExprSpec::ParameterLookup {
                parameter: parameter.clone(),
                index: Box::new(lower_to_scalar(index, ctx)?),
            }
        }
        Expr::BinOp { op, left, right } => {
            let l = lower_to_scalar(left, ctx)?;
            let r = lower_to_scalar(right, ctx)?;
            match op {
                BinOpKind::Add => ScalarExprSpec::Add { items: vec![l, r] },
                BinOpKind::Sub => ScalarExprSpec::Sub {
                    left: Box::new(l),
                    right: Box::new(r),
                },
                BinOpKind::Mul => ScalarExprSpec::Mul {
                    left: Box::new(l),
                    right: Box::new(r),
                },
                BinOpKind::Div => ScalarExprSpec::Div {
                    left: Box::new(l),
                    right: Box::new(r),
                },
                _ => {
                    return Err(FormulaError::lower(format!(
                        "binary op {:?} in scalar position",
                        op
                    )));
                }
            }
        }
        Expr::UnaryOp { op, operand } => match op {
            UnaryOpKind::Neg => ScalarExprSpec::Sub {
                left: Box::new(lit_int_spec(0)),
                right: Box::new(lower_to_scalar(operand, ctx)?),
            },
            UnaryOpKind::Not => {
                return Err(FormulaError::lower("`not` in scalar position".to_string()));
            }
        },
        Expr::Call { func, args } => match func.as_str() {
            "max" => ScalarExprSpec::Max {
                items: args
                    .iter()
                    .map(|a| lower_to_scalar(a, ctx))
                    .collect::<Result<_, _>>()?,
            },
            "min" => ScalarExprSpec::Min {
                items: args
                    .iter()
                    .map(|a| lower_to_scalar(a, ctx))
                    .collect::<Result<_, _>>()?,
            },
            "ceil" => {
                if args.len() != 1 {
                    return Err(FormulaError::lower("ceil takes 1 arg".to_string()));
                }
                ScalarExprSpec::Ceil {
                    value: Box::new(lower_to_scalar(&args[0], ctx)?),
                }
            }
            "floor" => {
                if args.len() != 1 {
                    return Err(FormulaError::lower("floor takes 1 arg".to_string()));
                }
                ScalarExprSpec::Floor {
                    value: Box::new(lower_to_scalar(&args[0], ctx)?),
                }
            }
            "days_between" => {
                if args.len() != 2 {
                    return Err(FormulaError::lower("days_between takes 2 args".to_string()));
                }
                ScalarExprSpec::DaysBetween {
                    from: Box::new(lower_to_scalar(&args[0], ctx)?),
                    to: Box::new(lower_to_scalar(&args[1], ctx)?),
                }
            }
            "date_add_days" => {
                if args.len() != 2 {
                    return Err(FormulaError::lower(
                        "date_add_days takes 2 args".to_string(),
                    ));
                }
                ScalarExprSpec::DateAddDays {
                    date: Box::new(lower_to_scalar(&args[0], ctx)?),
                    days: Box::new(lower_to_scalar(&args[1], ctx)?),
                }
            }
            // Over-periods reductions: reduce the inner expression across an
            // entity's own period axis. Meaningful only under the lifetime
            // execution surface; the per-period execution paths reject them.
            // The kind is chosen by an EXHAUSTIVE match with an explicit error
            // arm — never a wildcard falling through to `Count` — so a future
            // one-arg reduction added to this arm's guard cannot silently parse
            // as the period count. `sum_top_n_over_periods` (two args) is handled
            // separately below.
            "sum_over_periods" | "max_over_periods" | "count_over_periods" => {
                if args.len() != 1 {
                    return Err(FormulaError::lower(format!("{func} takes 1 arg")));
                }
                let over = match func.as_str() {
                    "sum_over_periods" => OverPeriodsKindSpec::Sum,
                    "max_over_periods" => OverPeriodsKindSpec::Max,
                    "count_over_periods" => OverPeriodsKindSpec::Count,
                    other => {
                        return Err(FormulaError::lower(format!(
                            "unknown one-argument over-periods reduction `{other}`"
                        )));
                    }
                };
                ScalarExprSpec::OverPeriods {
                    over,
                    value: Box::new(lower_to_scalar(&args[0], ctx)?),
                    n: None,
                }
            }
            "sum_top_n_over_periods" => {
                if args.len() != 2 {
                    return Err(FormulaError::lower(
                        "sum_top_n_over_periods takes 2 args".to_string(),
                    ));
                }
                ScalarExprSpec::OverPeriods {
                    over: OverPeriodsKindSpec::SumTopN,
                    value: Box::new(lower_to_scalar(&args[0], ctx)?),
                    n: Some(Box::new(lower_to_scalar(&args[1], ctx)?)),
                }
            }
            "len" => {
                let rel = match args.first() {
                    Some(Expr::Var(rel)) => rel.clone(),
                    Some(Expr::FieldAccess { obj, .. }) => match obj.as_ref() {
                        Expr::Var(rel) => rel.clone(),
                        _ => {
                            return Err(FormulaError::lower(
                                "len(...) requires a relation argument".to_string(),
                            ));
                        }
                    },
                    _ => {
                        return Err(FormulaError::lower(
                            "len(...) requires a relation argument".to_string(),
                        ));
                    }
                };
                ctx.relations.borrow_mut().insert(rel.clone());
                let (current_slot, related_slot) = infer_slots(&rel);
                return Ok(ScalarExprSpec::CountRelated {
                    relation: rel,
                    current_slot,
                    related_slot,
                    where_clause: None,
                });
            }
            "sum" => {
                if let Some(Expr::FieldAccess { obj, field }) = args.first() {
                    if let Expr::Var(rel) = obj.as_ref() {
                        ctx.relations.borrow_mut().insert(rel.clone());
                        let (current_slot, related_slot) = infer_slots(rel);
                        let value = if ctx.derived.contains(field) {
                            RelatedValueRefSpec::Derived {
                                name: field.clone(),
                            }
                        } else {
                            RelatedValueRefSpec::Input {
                                name: field.clone(),
                            }
                        };
                        return Ok(ScalarExprSpec::SumRelated {
                            relation: rel.clone(),
                            current_slot,
                            related_slot,
                            value,
                            where_clause: None,
                        });
                    }
                }
                return Err(FormulaError::lower(
                    "sum(...) requires a relation field access argument".to_string(),
                ));
            }
            // Extension: filtered aggregation via named calls, because the
            // deployed DSL has no lambda / predicate syntax on sum/len. Each
            // takes a relation and a Boolean input field that the caller
            // pre-computes on the related entity.
            //
            //   count_where(relation, predicate_field)
            //   sum_where(relation, amount_field_or_derived, predicate_field)
            "count_where" => {
                if args.len() != 2 {
                    return Err(FormulaError::lower("count_where takes 2 args".to_string()));
                }
                let relation = expect_var(&args[0], "count_where arg 1")?;
                let pred = expect_var(&args[1], "count_where arg 2")?;
                ctx.relations.borrow_mut().insert(relation.clone());
                let (current_slot, related_slot) = infer_slots(&relation);
                let where_clause = if ctx.derived.contains(&pred) {
                    JudgmentExprSpec::Derived { name: pred }
                } else {
                    JudgmentExprSpec::Comparison {
                        left: Box::new(ScalarExprSpec::Input { name: pred }),
                        op: ComparisonOpSpec::Eq,
                        right: Box::new(lit_bool_spec(true)),
                    }
                };
                ScalarExprSpec::CountRelated {
                    relation,
                    current_slot,
                    related_slot,
                    where_clause: Some(Box::new(where_clause)),
                }
            }
            "sum_where" => {
                if args.len() != 3 {
                    return Err(FormulaError::lower("sum_where takes 3 args".to_string()));
                }
                let relation = expect_var(&args[0], "sum_where arg 1")?;
                let field = expect_var(&args[1], "sum_where arg 2")?;
                let pred = expect_var(&args[2], "sum_where arg 3")?;
                ctx.relations.borrow_mut().insert(relation.clone());
                let (current_slot, related_slot) = infer_slots(&relation);
                let where_clause = if ctx.derived.contains(&pred) {
                    JudgmentExprSpec::Derived { name: pred }
                } else {
                    JudgmentExprSpec::Comparison {
                        left: Box::new(ScalarExprSpec::Input { name: pred }),
                        op: ComparisonOpSpec::Eq,
                        right: Box::new(lit_bool_spec(true)),
                    }
                };
                let value = if ctx.derived.contains(&field) {
                    RelatedValueRefSpec::Derived { name: field }
                } else {
                    RelatedValueRefSpec::Input { name: field }
                };
                ScalarExprSpec::SumRelated {
                    relation,
                    current_slot,
                    related_slot,
                    value,
                    where_clause: Some(Box::new(where_clause)),
                }
            }
            // Any other `*_over_periods` name is an unrecognized reduction, not a
            // generic unknown function: reject it explicitly so a typo or a
            // not-yet-implemented reduction fails to parse loudly instead of
            // being mistaken for a plain call.
            other if other.ends_with("_over_periods") => {
                return Err(FormulaError::lower(format!(
                    "unknown over-periods reduction `{other}` (expected one of sum_over_periods, max_over_periods, count_over_periods, sum_top_n_over_periods)"
                )));
            }
            _ => {
                return Err(FormulaError::lower(format!("unknown function `{}`", func)));
            }
        },
        Expr::Cond {
            condition,
            then_expr,
            else_expr,
        } => ScalarExprSpec::If {
            condition: Box::new(lower_to_judgment(condition, ctx)?),
            then_expr: Box::new(lower_to_scalar(then_expr, ctx)?),
            else_expr: Box::new(lower_to_scalar(else_expr, ctx)?),
        },
        Expr::Match { subject, cases } => {
            // Lower as nested if/elif: `match s: a => x, b => y, c => z` →
            // `if s == a: x elif s == b: y else: z`. The last case's value
            // becomes the else branch (rather than a default 0) so all
            // branches share the same dtype — the dense compiler enforces
            // branch-dtype equality.
            if cases.is_empty() {
                return Err(FormulaError::lower("empty match".to_string()));
            }
            let subj_scalar = lower_to_scalar(subject, ctx)?;
            let (last_pat, last_res) = cases.last().unwrap();
            let _ = last_pat; // last case is the fallthrough; pattern value unused
            let mut expr = lower_to_scalar(last_res, ctx)?;
            for (pat, res) in cases.iter().rev().skip(1) {
                expr = ScalarExprSpec::If {
                    condition: Box::new(JudgmentExprSpec::Comparison {
                        left: Box::new(subj_scalar.clone()),
                        op: ComparisonOpSpec::Eq,
                        right: Box::new(lower_to_scalar(pat, ctx)?),
                    }),
                    then_expr: Box::new(lower_to_scalar(res, ctx)?),
                    else_expr: Box::new(expr),
                };
            }
            expr
        }
        Expr::FieldAccess { .. } => {
            return Err(FormulaError::lower(
                "bare field access in scalar position not supported".to_string(),
            ));
        }
    })
}

fn expect_var(e: &Expr, ctx: &str) -> Result<String, FormulaError> {
    match e {
        Expr::Var(v) => Ok(v.clone()),
        _ => Err(FormulaError::lower(format!(
            "{} must be a variable name",
            ctx
        ))),
    }
}

/// Promote Integer literals to Decimal inside a Decimal-typed derived's
/// expression so `if`-branch dtypes match the declared output dtype. The
/// dense compiler refuses to mix Integer and Decimal branches; RuleSpec formulas allow
/// `1800` to read as either depending on surrounding unit, so we coerce
/// here. Traversal descends into arithmetic operators and `if` branches
/// but stops at comparisons (their operands compare naturally against the
/// other side's dtype) and at parameter-lookup indices (always Integer).
fn promote_ints_to_decimal(expr: &mut ScalarExprSpec) {
    match expr {
        ScalarExprSpec::Literal { value } => {
            if let ScalarValueSpec::Integer { value: v } = value {
                *value = ScalarValueSpec::Decimal {
                    value: v.to_string(),
                };
            }
        }
        ScalarExprSpec::Add { items }
        | ScalarExprSpec::Max { items }
        | ScalarExprSpec::Min { items } => {
            for item in items {
                promote_ints_to_decimal(item);
            }
        }
        ScalarExprSpec::Sub { left, right }
        | ScalarExprSpec::Mul { left, right }
        | ScalarExprSpec::Div { left, right } => {
            promote_ints_to_decimal(left);
            promote_ints_to_decimal(right);
        }
        ScalarExprSpec::Ceil { value } | ScalarExprSpec::Floor { value } => {
            promote_ints_to_decimal(value);
        }
        ScalarExprSpec::If {
            then_expr,
            else_expr,
            ..
        } => {
            promote_ints_to_decimal(then_expr);
            promote_ints_to_decimal(else_expr);
        }
        // A Sum / Max / SumTopN reduction is Decimal-typed, so promote integer
        // literals in its inner value to keep it Decimal. `n` (SumTopN count) is
        // an integer count and is deliberately left alone. Count reduces to an
        // integer, but promoting literals in its (ignored) inner value is
        // harmless, so we descend uniformly.
        ScalarExprSpec::OverPeriods { value, .. } => {
            promote_ints_to_decimal(value);
        }
        // Don't descend into:
        //   * Comparisons (operands compare by their own type)
        //   * ParameterLookup index (always Integer)
        //   * CountRelated / SumRelated (aggregators)
        //   * Input / Derived / PeriodStart/End / DateAddDays / DaysBetween
        _ => {}
    }
}

fn lower_to_judgment(e: &Expr, ctx: &LowerCtx) -> Result<JudgmentExprSpec, FormulaError> {
    Ok(match e {
        Expr::LitBool(b) => JudgmentExprSpec::Comparison {
            left: Box::new(lit_bool_spec(*b)),
            op: ComparisonOpSpec::Eq,
            right: Box::new(lit_bool_spec(true)),
        },
        Expr::Var(name) => {
            if ctx.derived.contains(name) {
                JudgmentExprSpec::Derived { name: name.clone() }
            } else if ctx.relation_predicates.contains(name) {
                let (current_slot, related_slot) = infer_slots(name);
                JudgmentExprSpec::RelationMember {
                    relation: name.clone(),
                    current_slot,
                    related_slot,
                }
            } else {
                JudgmentExprSpec::Comparison {
                    left: Box::new(ScalarExprSpec::Input { name: name.clone() }),
                    op: ComparisonOpSpec::Eq,
                    right: Box::new(lit_bool_spec(true)),
                }
            }
        }
        Expr::BinOp { op, left, right } => match op {
            BinOpKind::And => JudgmentExprSpec::And {
                items: vec![
                    lower_to_judgment(left, ctx)?,
                    lower_to_judgment(right, ctx)?,
                ],
            },
            BinOpKind::Or => JudgmentExprSpec::Or {
                items: vec![
                    lower_to_judgment(left, ctx)?,
                    lower_to_judgment(right, ctx)?,
                ],
            },
            BinOpKind::Lt
            | BinOpKind::Gt
            | BinOpKind::Le
            | BinOpKind::Ge
            | BinOpKind::Eq
            | BinOpKind::Ne => {
                let l = lower_to_scalar(left, ctx)?;
                let r = lower_to_scalar(right, ctx)?;
                let cmp = match op {
                    BinOpKind::Lt => ComparisonOpSpec::Lt,
                    BinOpKind::Gt => ComparisonOpSpec::Gt,
                    BinOpKind::Le => ComparisonOpSpec::Lte,
                    BinOpKind::Ge => ComparisonOpSpec::Gte,
                    BinOpKind::Eq => ComparisonOpSpec::Eq,
                    BinOpKind::Ne => ComparisonOpSpec::Ne,
                    _ => unreachable!(),
                };
                JudgmentExprSpec::Comparison {
                    left: Box::new(l),
                    op: cmp,
                    right: Box::new(r),
                }
            }
            _ => {
                return Err(FormulaError::lower(format!(
                    "binary op {:?} in judgment position",
                    op
                )));
            }
        },
        Expr::UnaryOp { op, operand } => match op {
            UnaryOpKind::Not => JudgmentExprSpec::Not {
                item: Box::new(lower_to_judgment(operand, ctx)?),
            },
            _ => {
                return Err(FormulaError::lower(
                    "unary op in judgment position".to_string(),
                ));
            }
        },
        _ => {
            return Err(FormulaError::lower(format!(
                "expression shape not supported in judgment position: {:?}",
                std::mem::discriminant(e)
            )));
        }
    })
}

/// Parse generated RuleSpec formula declarations and lower to a `ProgramSpec` ready for
/// the existing compile / execute pipeline.
pub(crate) fn lower_source(source: &str) -> Result<ProgramSpec, FormulaError> {
    let module = parse_source(source)?;
    lower_module(&module)
}

pub(crate) fn lower_judgment_formula(
    formula: &str,
    derived_names: HashSet<String>,
    relation_predicates: HashSet<String>,
) -> Result<JudgmentExprSpec, FormulaError> {
    let tokens = Lexer::new(formula).tokenise()?;
    let mut parser = Parser::new(tokens);
    let expr = parser.parse_expr()?;
    if parser.peek(0).ty != TokType::Eof {
        let tok = parser.peek(0);
        return Err(FormulaError::parse(
            tok.line,
            tok.col,
            "unexpected trailing token in relation predicate",
        ));
    }
    let ctx = LowerCtx {
        scalars: HashSet::new(),
        derived: derived_names,
        relation_predicates,
        relations: std::cell::RefCell::new(HashSet::new()),
    };
    lower_to_judgment(&expr, &ctx)
}
