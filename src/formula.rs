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
    ExtremeSpec, IndexedParameterSpec, JudgmentExprSpec, OverPeriodsKindSpec, ParameterVersionSpec,
    ProgramSpec, RelatedValueRefSpec, RelationSpec, ScalarExprSpec, ScalarValueSpec, TermSpec,
    UnitKindSpec, UnitSpec,
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

pub(crate) fn keyword(s: &str) -> Option<TokType> {
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
    text: &'a str,
    src: &'a [u8],
    pos: usize,
    line: usize,
    col: usize,
    tokens: Vec<Token>,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Self {
            text: src,
            src: src.as_bytes(),
            pos: 0,
            line: 1,
            col: 1,
            tokens: Vec::new(),
        }
    }

    /// The source text between two byte offsets. Scanning is bytewise, so an
    /// offset could land inside a multibyte character; that is a parse error
    /// here rather than a panic.
    fn slice(
        &self,
        start: usize,
        end: usize,
        line: usize,
        col: usize,
    ) -> Result<&'a str, FormulaError> {
        self.text.get(start..end).ok_or_else(|| {
            FormulaError::parse(
                line,
                col,
                format!("lexer offset {start}..{end} is not on a character boundary"),
            )
        })
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
                    let s = self.slice(self.pos, self.pos + 10, line, col)?.to_string();
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
                let s = self.slice(self.pos, end, line, col)?.to_string();
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
                        value.push_str(self.slice(segment_start, self.pos, line, col)?);
                        self.advance(1);
                        self.push(TokType::String, value, line, col);
                        break;
                    }
                    if current == b'\\' {
                        value.push_str(self.slice(segment_start, self.pos, line, col)?);
                        // Decode the whole escaped character: a multibyte one
                        // spans several bytes, and stepping over only its
                        // first would leave the cursor inside it.
                        let Some(escaped) = self
                            .slice(self.pos + 1, self.src.len(), line, col)?
                            .chars()
                            .next()
                        else {
                            return Err(FormulaError::parse(line, col, "unterminated string"));
                        };
                        value.push(match escaped {
                            'n' => '\n',
                            'r' => '\r',
                            't' => '\t',
                            // `\\`, `\"`, `\'`, and any other escaped
                            // character stand for that character.
                            other => other,
                        });
                        self.advance(1 + escaped.len_utf8());
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
                let s = self.slice(self.pos, final_end, line, col)?.to_string();
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
                    let s = self.slice(self.pos, self.pos + 2, line, col)?.to_string();
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
                    // Report the whole character, not its first UTF-8 byte
                    // read as Latin-1 (`é` would otherwise show as `Ã`).
                    let unexpected = self
                        .slice(self.pos, self.src.len(), line, col)?
                        .chars()
                        .next()
                        .unwrap_or(c as char);
                    return Err(FormulaError::parse(
                        line,
                        col,
                        format!("unexpected char {unexpected:?}"),
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
        default: Option<Box<Expr>>,
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
    pub end: Option<NaiveDate>,
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
            let end = if self.at(&[TokType::To]) {
                self.consume(TokType::To)?;
                let t = self.consume(TokType::Date)?;
                Some(
                    NaiveDate::parse_from_str(&t.value, "%Y-%m-%d")
                        .map_err(|e| FormulaError::parse(t.line, t.col, e.to_string()))?,
                )
            } else {
                None
            };
            self.consume(TokType::Colon)?;
            let expr = self.parse_expr()?;
            decl.values.push(TemporalValue { start, end, expr });
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
        let mut default = None;
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
            if default.is_some() {
                let tok = self.peek(0);
                return Err(FormulaError::parse(
                    tok.line,
                    tok.col,
                    "a match wildcard arm must be final",
                ));
            }
            let pat = self.parse_primary()?;
            self.consume(TokType::Arrow)?;
            let res = self.parse_expr()?;
            if matches!(&pat, Expr::Var(name) if name == "_") {
                default = Some(Box::new(res));
            } else {
                cases.push((pat, res));
            }
        }
        Ok(Expr::Match {
            subject: Box::new(subject),
            cases,
            default,
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
    if let Some(variable) = module
        .variables
        .iter()
        .find(|variable| variable.default.is_some())
    {
        return Err(FormulaError::lower(format!(
            "variable `{}` declares unsupported `default`; write an explicit formula fallback",
            variable.path
        )));
    }
    let mut program = ProgramSpec::default();

    // Helpful unit defaults so RuleSpec modules don't have to re-declare common
    // units inline. RuleSpec modules may override / add their own units by
    // declaring them, but RuleSpec formula declarations only carry unit names on
    // a variable declaration — we seed the spec with the commonly used ones.
    for (name, kind) in [
        ("GBP", UnitKindSpec::Currency { minor_units: 2 }),
        ("USD", UnitKindSpec::Currency { minor_units: 2 }),
        ("EUR", UnitKindSpec::Currency { minor_units: 2 }),
        // Swiss Franc: ISO 4217 exponent 2 (100 rappen = 1 franc).
        ("CHF", UnitKindSpec::Currency { minor_units: 2 }),
        // Norwegian Krone: ISO 4217 exponent 2 (100 øre = 1 krone).
        ("NOK", UnitKindSpec::Currency { minor_units: 2 }),
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
        // Rwandan Franc: ISO 4217 lists no minor unit in circulation
        // (exponent 0); Rwandan laws state amounts in whole francs.
        ("RWF", UnitKindSpec::Currency { minor_units: 0 }),
        // Tanzanian Shilling: ISO 4217 exponent 2 (100 senti = 1
        // shilingi); Tanzanian statutes state amounts in shillings.
        ("TZS", UnitKindSpec::Currency { minor_units: 2 }),
        // Danish Krone: ISO 4217 exponent 2 (100 øre = 1 krone);
        // Danish statutes state amounts in whole kroner.
        ("DKK", UnitKindSpec::Currency { minor_units: 2 }),
        // Armenian Dram: ISO 4217 exponent 2 (100 luma = 1 dram);
        // Armenian statutes state amounts in drams.
        ("AMD", UnitKindSpec::Currency { minor_units: 2 }),
        // New Israeli Shekel: ISO 4217 exponent 2 (100 agorot = 1
        // shekel); Israeli statutes state amounts in shekels.
        ("ILS", UnitKindSpec::Currency { minor_units: 2 }),
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
    let scalar_names = module
        .variables
        .iter()
        .filter(|variable| is_parameter_variable(variable))
        .map(|variable| variable.path.clone())
        .collect::<HashSet<String>>();
    let derived_names = module
        .variables
        .iter()
        .filter(|variable| !is_parameter_variable(variable))
        .map(|variable| variable.path.clone())
        .collect::<HashSet<String>>();

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
                effective_to: t.end,
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
        terms: false,
        reserved_variables: HashSet::new(),
        variables: std::cell::RefCell::new(Vec::new()),
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
        let versions = v
            .values
            .iter()
            .map(|value| {
                Ok(DerivedVersionSpec {
                    effective_from: value.start,
                    effective_to: value.end,
                    semantics: lower_semantics(&value.expr, &dtype)?,
                })
            })
            .collect::<Result<Vec<DerivedVersionSpec>, FormulaError>>()?;
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
            slot_entities: Vec::new(),
            derivation: None,
        });
    }

    Ok(program)
}

/// Legacy slot convention for two-slot relations referenced from aggregations.
///
/// Formula lowering has no entity-kind context, so it cannot infer direction
/// from a relation's declared argument entities here. RuleSpec later carries
/// those declarations into the relation schema and resolves unambiguous
/// aggregation directions after relation aliases have been rewritten.
fn infer_slots(_relation: &str) -> (usize, usize) {
    // Preserve the established runtime convention: current entity in slot 1,
    // related entity in slot 0.
    (1, 0)
}

pub(crate) fn infer_relation_slots_for_rulespec(relation: &str) -> (usize, usize) {
    infer_slots(relation)
}

/// Whether lowering reads `variable` as a parameter: it has no entity and
/// every value is a literal. A table parameter is written as such a stub.
fn is_parameter_variable(variable: &VariableDecl) -> bool {
    variable.entity.is_none()
        && variable
            .values
            .iter()
            .all(|value| is_literal_expr(&value.expr))
}

/// The names `lower_module` reads as parameters in `source`: the parameter
/// tables, the scalar parameters, and the entity-free rules with literal
/// values. Derived-relation predicates read the same set (relation roles,
/// section 3.3).
pub(crate) fn parameter_names(source: &str) -> Result<HashSet<String>, FormulaError> {
    Ok(parse_source(source)?
        .variables
        .iter()
        .filter(|variable| is_parameter_variable(variable))
        .map(|variable| variable.path.clone())
        .collect())
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
    /// Lowering a derived relation's predicate, where `anchor` and
    /// `candidate` are reserved terms. Elsewhere they stay ordinary names.
    terms: bool,
    /// Every name bound anywhere in this predicate is reserved, including
    /// outside the body that binds it. The active stack is separate so a
    /// reserved name never silently becomes an input read outside its scope.
    reserved_variables: HashSet<String>,
    variables: std::cell::RefCell<Vec<String>>,
    // Relations discovered while lowering expressions (len / sum /
    // count_where / sum_where) so we can emit matching RelationSpec
    // declarations without requiring an explicit entity-declaration
    // block in the source.
    relations: std::cell::RefCell<HashSet<String>>,
}

/// The term a reserved predicate name stands for.
fn term_named(name: &str) -> Option<TermSpec> {
    match name {
        "anchor" => Some(TermSpec::Anchor),
        "candidate" => Some(TermSpec::Candidate),
        _ => None,
    }
}

/// Function names cannot also be used for relation atoms. `builtin_tests`
/// keeps this list equal to the scalar and judgment call lowering below.
const BUILTIN_FUNCTIONS: [&str; 23] = [
    "max",
    "min",
    "ceil",
    "floor",
    "days_between",
    "date_add_days",
    "date_add_months",
    "date_add_years",
    "sum_over_periods",
    "max_over_periods",
    "count_over_periods",
    "sum_top_n_over_periods",
    "len",
    "sum",
    "only",
    "input_for_year",
    "only_related",
    "count_where",
    "sum_where",
    "min_where",
    "max_where",
    "exactly_one",
    "exists",
];

pub(crate) fn is_builtin_function(name: &str) -> bool {
    BUILTIN_FUNCTIONS.contains(&name)
}

/// Whether `func(args)` is a relation atom. A call to a built-in function name
/// stays the built-in, even when a relation has that name, unless it passes a
/// bare term, which only an atom takes: that call shadows the built-in.
fn is_relation_call(relations: &HashSet<String>, func: &str, args: &[Expr]) -> bool {
    relations.contains(func)
        && (!is_builtin_function(func)
            || args
                .iter()
                .any(|arg| matches!(arg, Expr::Var(name) if term_named(name).is_some())))
}

impl LowerCtx {
    /// The term `name` stands for, when this is a predicate.
    fn term(&self, name: &str) -> Option<TermSpec> {
        if !self.terms {
            return None;
        }
        term_named(name).or_else(|| {
            self.variables
                .borrow()
                .iter()
                .rev()
                .position(|variable| variable == name)
                .map(TermSpec::Var)
        })
    }

    fn is_relation_call(&self, func: &str, args: &[Expr]) -> bool {
        is_relation_call(&self.relation_predicates, func, args)
            || (self.relation_predicates.contains(func)
                && args
                    .iter()
                    .any(|arg| matches!(arg, Expr::Var(name) if self.term(name).is_some())))
    }

    /// The term `expr` reads through, when it is `t.name` in a predicate.
    fn role_read<'e>(&self, expr: &'e Expr) -> Option<(TermSpec, &'e str)> {
        let Expr::FieldAccess { obj, field } = expr else {
            return None;
        };
        let Expr::Var(name) = obj.as_ref() else {
            return None;
        };
        Some((self.term(name)?, field.as_str()))
    }

    /// Refuse a reserved term written where a name is read (`what`).
    fn reject_term_name(&self, name: &str, what: &str) -> Result<(), FormulaError> {
        self.reject_out_of_scope_name(name)?;
        match self.term(name) {
            Some(term) => Err(term_used_as_value(term, what)),
            None => Ok(()),
        }
    }

    fn reject_out_of_scope_name(&self, name: &str) -> Result<(), FormulaError> {
        if self.terms && self.reserved_variables.contains(name) && self.term(name).is_none() {
            return Err(FormulaError::lower(format!(
                "[relation_variable_out_of_scope] `{name}` is bound by an `exists` elsewhere in this predicate and is only available in that `exists` body"
            )));
        }
        Ok(())
    }

    /// Check the object of a qualified read before deciding whether it is a
    /// role read or an aggregation field. Neither interpretation may turn a
    /// variable outside its body into an ordinary input or relation name.
    fn reject_out_of_scope_read(&self, expr: &Expr) -> Result<(), FormulaError> {
        match expr {
            Expr::Var(name) => self.reject_out_of_scope_name(name),
            Expr::FieldAccess { obj, .. } => match obj.as_ref() {
                Expr::Var(name) => self.reject_out_of_scope_name(name),
                _ => Ok(()),
            },
            _ => Ok(()),
        }
    }

    /// Refuse a reserved term, or a read through one, as an argument of an
    /// aggregation (`what`): no term is bound there.
    fn reject_in_aggregation(&self, arg: &Expr, what: &str) -> Result<(), FormulaError> {
        self.reject_out_of_scope_read(arg)?;
        if self.scoped_construct(arg) {
            return Err(FormulaError::lower(format!(
                "[relation_role_outside_scope] {what} has no term bindings; qualified reads, atoms and term comparisons require the derived relation's predicate scope"
            )));
        }
        if let Some((term, name)) = self.role_read(arg) {
            return Err(FormulaError::lower(format!(
                "[relation_role_outside_scope] `{}.{name}` is read as {what}, which is evaluated on the aggregated relation's ids, where no term is bound; a read through a term may appear in a derived relation's predicate only outside any aggregation's `where` clause or aggregated value",
                term.as_str()
            )));
        }
        if let Expr::Var(name) = arg {
            self.reject_term_name(name, what)?;
        }
        Ok(())
    }

    fn scoped_construct(&self, expr: &Expr) -> bool {
        if self.role_read(expr).is_some() {
            return true;
        }
        match expr {
            Expr::Call { func, args } => {
                func == "exists"
                    || self.is_relation_call(func, args)
                    || args.iter().any(|arg| self.scoped_construct(arg))
            }
            Expr::BinOp { left, right, .. } => {
                let term =
                    |expr: &Expr| matches!(expr, Expr::Var(name) if self.term(name).is_some());
                term(left)
                    || term(right)
                    || self.scoped_construct(left)
                    || self.scoped_construct(right)
            }
            Expr::UnaryOp { operand, .. } => self.scoped_construct(operand),
            Expr::FieldAccess { obj, .. } => self.scoped_construct(obj),
            Expr::Index { target, index } => {
                self.scoped_construct(target) || self.scoped_construct(index)
            }
            Expr::Cond {
                condition,
                then_expr,
                else_expr,
            } => {
                self.scoped_construct(condition)
                    || self.scoped_construct(then_expr)
                    || self.scoped_construct(else_expr)
            }
            Expr::Match {
                subject,
                cases,
                default,
            } => {
                self.scoped_construct(subject)
                    || cases.iter().any(|(pattern, value)| {
                        self.scoped_construct(pattern) || self.scoped_construct(value)
                    })
                    || default
                        .as_deref()
                        .is_some_and(|value| self.scoped_construct(value))
            }
            Expr::LitInt(_)
            | Expr::LitFloat(_)
            | Expr::LitStr(_)
            | Expr::LitBool(_)
            | Expr::Var(_) => false,
        }
    }

    fn lower_exists(&self, args: &[Expr]) -> Result<JudgmentExprSpec, FormulaError> {
        if !self.terms {
            return Err(FormulaError::lower(
                "[relation_role_outside_scope] `exists` requires a derived relation predicate",
            ));
        }
        if args.len() != 2 {
            return Err(FormulaError::lower(format!(
                "[relation_exists_arity_mismatch] `exists` takes a generator atom and a body, got {} arguments",
                args.len()
            )));
        }
        let Expr::Call {
            func,
            args: generator_args,
        } = &args[0]
        else {
            return Err(FormulaError::lower(
                "[relation_exists_generator_not_atom] the first `exists` argument must be an atom over a declared relation",
            ));
        };
        if !self.relation_predicates.contains(func) {
            return Err(FormulaError::lower(format!(
                "[relation_exists_generator_not_atom] `{func}` is not a declared relation"
            )));
        }
        if is_builtin_function(func) {
            return Err(FormulaError::lower(format!(
                "[relation_name_shadows_builtin] relation `{func}` also names a built-in function"
            )));
        }
        let mut terms = Vec::with_capacity(generator_args.len());
        let mut variable = None;
        let mut existing_names = HashSet::new();
        let mut rebinding = None;
        for (slot, arg) in generator_args.iter().enumerate() {
            let Expr::Var(name) = arg else {
                return Err(FormulaError::lower(
                    "[relation_atom_argument_not_term] a generator argument must be a term in scope or its new variable name",
                ));
            };
            if let Some(term) = self.term(name) {
                if (!existing_names.insert(name) || matches!(term, TermSpec::Var(_)))
                    && rebinding.is_none()
                {
                    rebinding = Some(name);
                }
                terms.push(term);
                continue;
            }
            if self.scalars.contains(name)
                || self.derived.contains(name)
                || self.relation_predicates.contains(name)
            {
                return Err(FormulaError::lower(format!(
                    "[relation_variable_name_conflict] `{name}` already names a rule, parameter or relation visible to this module"
                )));
            }
            if !lowercase_identifier(name) {
                return Err(FormulaError::lower(format!(
                    "[relation_variable_invalid_name] `{name}` is not a lowercase identifier"
                )));
            }
            if variable.is_some() {
                return Err(FormulaError::lower(
                    "[relation_exists_variable_count] a generator must contain exactly one new variable argument, but has more than one",
                ));
            }
            variable = Some((slot, name.clone()));
            // The generator is evaluated in its enclosing environment. This
            // slot alone is a placeholder; the new frame exists in the body.
            terms.push(TermSpec::Var(0));
        }
        let Some((variable_slot, name)) = variable else {
            if let Some(name) = rebinding {
                return Err(FormulaError::lower(format!(
                    "[relation_variable_name_conflict] generator has no new variable and would rebind `{name}`, which is already a term in scope"
                )));
            }
            return Err(FormulaError::lower(
                "[relation_exists_variable_count] a generator must contain exactly one new variable argument; every argument is already a term in scope",
            ));
        };
        self.variables.borrow_mut().push(name);
        let body = lower_to_judgment(&args[1], self);
        self.variables.borrow_mut().pop();
        Ok(JudgmentExprSpec::Exists {
            relation: func.clone(),
            terms,
            variable_slot,
            body: Box::new(body?),
        })
    }

    /// Lower `term.name`, a read of a derived rule or an input at the term's
    /// id (relation roles, section 3.2).
    fn lower_role_read(&self, term: TermSpec, name: &str) -> Result<ScalarExprSpec, FormulaError> {
        let t = term.as_str();
        self.reject_term_name(name, &format!("the name read through `{t}`"))?;
        if self.relation_predicates.contains(name) {
            return Err(FormulaError::lower(format!(
                "`{t}.{name}` reads relation `{name}`; a read through a term names a rule or an input; test the relation with an atom"
            )));
        }
        if self.scalars.contains(name) || matches!(name, "period_start" | "period_end") {
            return Err(FormulaError::lower(format!(
                "[relation_role_entity_mismatch] `{t}.{name}` reads `{name}`, which has no entity, so it reads no id; read it unqualified"
            )));
        }
        Ok(if self.derived.contains(name) {
            ScalarExprSpec::RoleDerived {
                term,
                name: name.to_string(),
            }
        } else {
            ScalarExprSpec::RoleInput {
                term,
                name: name.to_string(),
            }
        })
    }
}

/// The refusal of a reserved term written as a value (`what`).
fn term_used_as_value(term: TermSpec, what: &str) -> FormulaError {
    FormulaError::lower(format!(
        "[relation_term_used_as_value] `{}` names a term of this derived relation's predicate, so it cannot be {what}; read a value at it with `{}.name`",
        term.as_str(),
        term.as_str()
    ))
}

fn lowercase_identifier(name: &str) -> bool {
    let mut chars = name.bytes();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
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
    ctx.reject_out_of_scope_read(e)?;
    if let Expr::Call { func, args } = e {
        if ctx.is_relation_call(func, args) {
            let (code, detail) = if is_builtin_function(func) {
                (
                    "relation_name_shadows_builtin",
                    "also names a built-in function",
                )
            } else {
                (
                    "relation_atom_used_as_value",
                    "is a judgment, so it cannot be used as a scalar value",
                )
            };
            return Err(FormulaError::lower(format!(
                "[{code}] relation `{func}` {detail}"
            )));
        }
    }
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
        Expr::Var(name) => {
            ctx.reject_term_name(name, "read as a value")?;
            match name.as_str() {
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
            }
        }
        Expr::Index { target, index } => {
            let Expr::Var(parameter) = target.as_ref() else {
                return Err(FormulaError::lower(
                    "parameter lookup target must be a named parameter".to_string(),
                ));
            };
            ctx.reject_term_name(parameter, "indexed as a parameter table")?;
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
        Expr::Call { func, args } if args.is_empty() && matches!(func.as_str(), "max" | "min") => {
            return Err(FormulaError::lower(format!(
                "{func}() requires at least one operand"
            )));
        }
        // `min(relation.field)` / `max(relation.field)`: with a single
        // relation field access argument these aggregate over the related
        // entities. Every other form keeps its scalar meaning below. In a
        // predicate, `anchor` and `candidate` name terms, not relations, so
        // `max(candidate.age)` is the scalar maximum of one qualified read.
        Expr::Call { func, args }
            if matches!(func.as_str(), "max" | "min")
                && args.len() == 1
                && relation_field_access(&args[0])
                    .is_some_and(|(relation, _)| ctx.term(relation).is_none()) =>
        {
            let (relation, field) = relation_field_access(&args[0]).expect("checked by the guard");
            ctx.reject_out_of_scope_name(relation)?;
            ctx.reject_term_name(field, "the aggregated value")?;
            let extreme = if func == "min" {
                ExtremeSpec::Min
            } else {
                ExtremeSpec::Max
            };
            ctx.relations.borrow_mut().insert(relation.to_string());
            let (current_slot, related_slot) = infer_slots(relation);
            ScalarExprSpec::ExtremeRelated {
                extreme,
                relation: relation.to_string(),
                current_slot,
                related_slot,
                value: related_value_ref(field, ctx),
                where_clause: None,
            }
        }
        Expr::Call { func, args } => match func.as_str() {
            "exists" => {
                if !ctx.terms {
                    return Err(FormulaError::lower(
                        "[relation_role_outside_scope] `exists` requires a derived relation predicate",
                    ));
                }
                return Err(FormulaError::lower(
                    "[relation_exists_used_as_value] `exists` is a judgment and cannot be used as a scalar value",
                ));
            }
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
            "date_add_months" => {
                if args.len() != 2 {
                    return Err(FormulaError::lower(
                        "date_add_months takes 2 args".to_string(),
                    ));
                }
                ScalarExprSpec::DateAddMonths {
                    date: Box::new(lower_to_scalar(&args[0], ctx)?),
                    months: Box::new(lower_to_scalar(&args[1], ctx)?),
                }
            }
            "date_add_years" => {
                if args.len() != 2 {
                    return Err(FormulaError::lower(
                        "date_add_years takes 2 args".to_string(),
                    ));
                }
                ScalarExprSpec::DateAddYears {
                    date: Box::new(lower_to_scalar(&args[0], ctx)?),
                    years: Box::new(lower_to_scalar(&args[1], ctx)?),
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
                ctx.reject_term_name(&rel, "counted as a relation")?;
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
                if let Some(arg) = args.first() {
                    ctx.reject_in_aggregation(arg, "the summed value")?;
                }
                if let Some(Expr::FieldAccess { obj, field }) = args.first() {
                    if let Expr::Var(rel) = obj.as_ref() {
                        ctx.reject_term_name(field, "the summed value")?;
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
            // `only(relation.field)`: the field of the one related entity.
            // Zero or several related entities fail at evaluation; it never
            // reads a default.
            "only" => {
                if args.len() != 1 {
                    return Err(FormulaError::lower("only takes 1 arg".to_string()));
                }
                ctx.reject_in_aggregation(&args[0], "the value `only` reads")?;
                let Some((relation, field)) = relation_field_access(&args[0]) else {
                    return Err(FormulaError::lower(
                        "only(...) requires a relation field access argument".to_string(),
                    ));
                };
                ctx.reject_term_name(field, "the value `only` reads")?;
                ctx.relations.borrow_mut().insert(relation.to_string());
                let (current_slot, related_slot) = infer_slots(relation);
                ScalarExprSpec::SingleRelated {
                    relation: relation.to_string(),
                    current_slot,
                    related_slot,
                    value: related_value_ref(field, ctx),
                    where_clause: None,
                }
            }
            // Extension: filtered aggregation via named calls, because the
            // deployed DSL has no lambda / predicate syntax on sum/len. Each
            // takes a relation and a Boolean input field that the caller
            // pre-computes on the related entity.
            //
            //   count_where(relation, predicate_field)
            //   sum_where(relation, amount_field_or_derived, predicate_field)
            "input_for_year" => {
                if args.len() != 2 {
                    return Err(FormulaError::lower("input_for_year takes 2 args"));
                }
                let input = expect_var(&args[0], "input_for_year primitive input")?;
                ctx.reject_term_name(&input, "read as an input")?;
                if ctx.derived.contains(&input) || ctx.scalars.contains(&input) {
                    return Err(FormulaError::lower(
                        "input_for_year requires a declared primitive",
                    ));
                }
                let Expr::LitInt(year) = args[1] else {
                    return Err(FormulaError::lower(
                        "input_for_year initially requires an integer year literal",
                    ));
                };
                if !(1..=9999).contains(&year) {
                    return Err(FormulaError::lower("input_for_year year must be 1..9999"));
                }
                ScalarExprSpec::InputForYear {
                    input,
                    year: year as i32,
                }
            }
            "only_related" => {
                if args.len() != 2 {
                    return Err(FormulaError::lower("only_related takes 2 args"));
                }
                ctx.reject_in_aggregation(&args[0], "the relation `only_related` reads")?;
                ctx.reject_in_aggregation(&args[1], "the input `only_related` projects")?;
                let relation = expect_var(&args[0], "only_related relation")?;
                let field = expect_var(&args[1], "only_related Boolean input")?;
                if ctx.derived.contains(&field) {
                    return Err(FormulaError::lower(
                        "only_related requires an input, not a derived rule",
                    ));
                }
                ctx.relations.borrow_mut().insert(relation.clone());
                let (current_slot, related_slot) = infer_slots(&relation);
                ScalarExprSpec::OnlyRelated {
                    relation,
                    current_slot,
                    related_slot,
                    value: RelatedValueRefSpec::Input { name: field },
                }
            }
            "count_where" => {
                if args.len() != 2 {
                    return Err(FormulaError::lower("count_where takes 2 args".to_string()));
                }
                ctx.reject_in_aggregation(&args[0], "the relation `count_where` counts")?;
                ctx.reject_in_aggregation(&args[1], "a `count_where` `where` clause")?;
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
                ctx.reject_in_aggregation(&args[0], "the relation `sum_where` sums over")?;
                ctx.reject_in_aggregation(&args[1], "the summed value")?;
                ctx.reject_in_aggregation(&args[2], "a `sum_where` `where` clause")?;
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
            //   min_where(relation, field, predicate_field)
            //   max_where(relation, field, predicate_field)
            //
            // The extreme of `field` over the related entities for which the
            // Boolean `predicate_field` holds, with `sum_where`'s argument
            // forms and resolution.
            "min_where" | "max_where" => {
                if args.len() != 3 {
                    return Err(FormulaError::lower(format!("{func} takes 3 args")));
                }
                ctx.reject_in_aggregation(&args[0], &format!("the relation `{func}` reads"))?;
                ctx.reject_in_aggregation(&args[1], "the aggregated value")?;
                ctx.reject_in_aggregation(&args[2], &format!("a `{func}` `where` clause"))?;
                let extreme = if func == "min_where" {
                    ExtremeSpec::Min
                } else {
                    ExtremeSpec::Max
                };
                let relation = expect_var(&args[0], &format!("{func} arg 1"))?;
                let field = expect_var(&args[1], &format!("{func} arg 2"))?;
                let pred = expect_var(&args[2], &format!("{func} arg 3"))?;
                ctx.relations.borrow_mut().insert(relation.clone());
                let (current_slot, related_slot) = infer_slots(&relation);
                ScalarExprSpec::ExtremeRelated {
                    extreme,
                    relation,
                    current_slot,
                    related_slot,
                    value: related_value_ref(&field, ctx),
                    where_clause: Some(Box::new(related_predicate(pred, ctx))),
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
        Expr::Match {
            subject,
            cases,
            default,
        } => {
            // Lower every concrete arm as a comparison. A final `_ => ...`
            // arm is the explicit fallback. A match without `_` ends its
            // chain in NoMatch, so a subject no pattern covers is an
            // evaluation error rather than the last arm's value; RuleSpec
            // lowering still warns about it (or rejects it in strict mode).
            if cases.is_empty() && default.is_none() {
                return Err(FormulaError::lower("empty match".to_string()));
            }
            let subj_scalar = lower_to_scalar(subject, ctx)?;
            let mut expr = match default {
                Some(default) => lower_to_scalar(default, ctx)?,
                None => ScalarExprSpec::NoMatch {
                    subject: Box::new(subj_scalar.clone()),
                    patterns: cases
                        .iter()
                        .map(|(pattern, _)| lower_to_scalar(pattern, ctx))
                        .collect::<Result<_, _>>()?,
                },
            };
            for (pat, res) in cases.iter().rev() {
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
            let Some((term, name)) = ctx.role_read(e) else {
                return Err(FormulaError::lower(
                    "bare field access in scalar position not supported".to_string(),
                ));
            };
            ctx.lower_role_read(term, name)?
        }
    })
}

/// The relation and field of a `relation.field` argument, or `None` for any
/// other form.
fn relation_field_access(e: &Expr) -> Option<(&str, &str)> {
    match e {
        Expr::FieldAccess { obj, field } => match obj.as_ref() {
            Expr::Var(relation) => Some((relation.as_str(), field.as_str())),
            _ => None,
        },
        _ => None,
    }
}

/// A related entity's field as an aggregation reads it: its derived rule when
/// one has that name, else its input.
fn related_value_ref(field: &str, ctx: &LowerCtx) -> RelatedValueRefSpec {
    if ctx.derived.contains(field) {
        RelatedValueRefSpec::Derived {
            name: field.to_string(),
        }
    } else {
        RelatedValueRefSpec::Input {
            name: field.to_string(),
        }
    }
}

/// A `*_where` predicate on the related entity: its derived judgment when one
/// has that name, else its Boolean input compared with `true`.
fn related_predicate(pred: String, ctx: &LowerCtx) -> JudgmentExprSpec {
    if ctx.derived.contains(&pred) {
        JudgmentExprSpec::Derived { name: pred }
    } else {
        JudgmentExprSpec::Comparison {
            left: Box::new(ScalarExprSpec::Input { name: pred }),
            op: ComparisonOpSpec::Eq,
            right: Box::new(lit_bool_spec(true)),
        }
    }
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
        //   * CountRelated / SumRelated / ExtremeRelated / SingleRelated
        //     (aggregators)
        //   * Input / Derived / PeriodStart/End / DateAddDays / DaysBetween
        //   * RoleInput / RoleDerived (reads, like Input / Derived)
        _ => {}
    }
}

fn lower_to_judgment(e: &Expr, ctx: &LowerCtx) -> Result<JudgmentExprSpec, FormulaError> {
    ctx.reject_out_of_scope_read(e)?;
    // A relation call is a judgment, with arguments in declared tuple order.
    if let Expr::Call { func, args } = e {
        if ctx.is_relation_call(func, args) {
            if !ctx.terms {
                return Err(FormulaError::lower(
                    "[relation_role_outside_scope] relation atoms require a derived relation predicate",
                ));
            }
            if is_builtin_function(func) {
                return Err(FormulaError::lower(format!(
                    "[relation_name_shadows_builtin] relation `{func}` also names a built-in function"
                )));
            }
            let terms = args
                .iter()
                .map(|arg| match arg {
                    Expr::Var(name) => {
                        ctx.reject_out_of_scope_name(name)?;
                        ctx.term(name).ok_or_else(|| {
                            FormulaError::lower(format!(
                                "[relation_atom_argument_not_term] `{name}` is not a term in scope"
                            ))
                        })
                    }
                    _ => Err(FormulaError::lower(
                        "[relation_atom_argument_not_term] every atom argument must be a term",
                    )),
                })
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(JudgmentExprSpec::RelationAtom {
                relation: func.clone(),
                terms,
            });
        }
        // Ordinary rule lowering has no declared-relation namespace. A call
        // with term arguments was already invalid there; give it the scope
        // refusal instead of an unknown-function diagnostic.
        if !ctx.terms
            && !is_builtin_function(func)
            && args
                .iter()
                .any(|arg| matches!(arg, Expr::Var(name) if term_named(name).is_some()))
        {
            return Err(FormulaError::lower(
                "[relation_role_outside_scope] relation atoms require a derived relation predicate",
            ));
        }
    }
    // Only a comparison compares terms. Under `and`, `or` or arithmetic a bare
    // term is refused where it is lowered, as a judgment or as a value, rather
    // than as a comparison it is not.
    if let Expr::BinOp { op, left, right } = e
        && matches!(
            op,
            BinOpKind::Lt
                | BinOpKind::Gt
                | BinOpKind::Le
                | BinOpKind::Ge
                | BinOpKind::Eq
                | BinOpKind::Ne
        )
    {
        ctx.reject_out_of_scope_read(left)?;
        ctx.reject_out_of_scope_read(right)?;
        let term = |expr: &Expr| match expr {
            Expr::Var(name) => ctx.term(name),
            _ => None,
        };
        let pair = (term(left), term(right));
        if pair.0.is_some() || pair.1.is_some() {
            let (Some(left), Some(right)) = pair else {
                return Err(FormulaError::lower(
                    "[relation_term_compared_with_value] an entity term can only be compared with another term",
                ));
            };
            if !matches!(op, BinOpKind::Eq | BinOpKind::Ne) {
                return Err(FormulaError::lower(
                    "[relation_term_comparison_operator] entity terms support only == and !=",
                ));
            }
            return Ok(JudgmentExprSpec::SameEntity {
                left,
                right,
                negated: matches!(op, BinOpKind::Ne),
            });
        }
    }
    // As with a bare name, a rule read through a term is a judgment and an
    // input read through one holds when it is true.
    if let Some((term, name)) = ctx.role_read(e) {
        return Ok(match ctx.lower_role_read(term, name)? {
            ScalarExprSpec::RoleDerived { term, name } => {
                JudgmentExprSpec::RoleDerived { term, name }
            }
            read => JudgmentExprSpec::Comparison {
                left: Box::new(read),
                op: ComparisonOpSpec::Eq,
                right: Box::new(lit_bool_spec(true)),
            },
        });
    }
    Ok(match e {
        Expr::LitBool(b) => JudgmentExprSpec::Comparison {
            left: Box::new(lit_bool_spec(*b)),
            op: ComparisonOpSpec::Eq,
            right: Box::new(lit_bool_spec(true)),
        },
        Expr::Var(name) => {
            ctx.reject_term_name(name, "tested as a judgment")?;
            // A bare name resolves in `lower_to_scalar`'s order. The period
            // built-ins come first and are dates, so neither is a judgment,
            // whatever else shares the name; a parameter holds when it is
            // true.
            if matches!(name.as_str(), "period_start" | "period_end") {
                return Err(FormulaError::lower(format!(
                    "`{name}` is a date, so it cannot be tested as a judgment; compare it, as in `days_between(period_start, period_end) > 0`"
                )));
            }
            if ctx.scalars.contains(name) {
                JudgmentExprSpec::Comparison {
                    left: Box::new(ScalarExprSpec::ParameterLookup {
                        parameter: name.clone(),
                        index: Box::new(lit_int_spec(0)),
                    }),
                    op: ComparisonOpSpec::Eq,
                    right: Box::new(lit_bool_spec(true)),
                }
            } else if ctx.derived.contains(name) {
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
        // `exactly_one(a, b, ...)` lowers to a first-class spec node, so
        // artifacts and graph consumers carry one n-input gate instead of the
        // ~n² expansion. The spec's `to_model` performs the same flat
        // Or-of-And-of-Nots lowering this arm used to do inline, so
        // evaluation, short-circuit reads, missing-input faults, and trace
        // text are unchanged from the pre-first-class sugar. Outcomes and
        // faults also match hand-written expansions on every input (the
        // exactly_one integration tests check exactly that); trace text does
        // not match the manual form byte-for-byte, because hand-chained
        // and/or nest binary while this lowering is flat n-ary.
        Expr::Call { func, args } => match func.as_str() {
            "exists" => ctx.lower_exists(args)?,
            "exactly_one" => {
                if args.len() < 2 {
                    return Err(FormulaError::lower(format!(
                        "exactly_one takes at least 2 arguments, got {}",
                        args.len()
                    )));
                }
                JudgmentExprSpec::ExactlyOne {
                    items: args
                        .iter()
                        .map(|arg| lower_to_judgment(arg, ctx))
                        .collect::<Result<Vec<_>, _>>()?,
                }
            }
            other => {
                return Err(FormulaError::lower(format!(
                    "unknown function `{other}` in judgment position"
                )));
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

/// Count `match` expressions that do not declare a final `_ => ...` arm.
///
/// RuleSpec uses this before lowering so compatibility mode can surface a
/// warning while strict mode rejects the same source. Parsing is shared with
/// normal formula lowering, so diagnostics cannot drift from the accepted
/// expression grammar.
pub(crate) fn non_exhaustive_match_count(formula: &str) -> Result<usize, FormulaError> {
    let tokens = Lexer::new(formula).tokenise()?;
    let mut parser = Parser::new(tokens);
    let expr = parser.parse_expr()?;
    if parser.peek(0).ty != TokType::Eof {
        let tok = parser.peek(0);
        return Err(FormulaError::parse(
            tok.line,
            tok.col,
            "unexpected trailing token in formula",
        ));
    }
    Ok(count_non_exhaustive_matches(&expr))
}

fn count_non_exhaustive_matches(expr: &Expr) -> usize {
    match expr {
        Expr::LitInt(_) | Expr::LitFloat(_) | Expr::LitStr(_) | Expr::LitBool(_) | Expr::Var(_) => {
            0
        }
        Expr::BinOp { left, right, .. } => {
            count_non_exhaustive_matches(left) + count_non_exhaustive_matches(right)
        }
        Expr::UnaryOp { operand, .. } => count_non_exhaustive_matches(operand),
        Expr::Call { args, .. } => args.iter().map(count_non_exhaustive_matches).sum(),
        Expr::FieldAccess { obj, .. } => count_non_exhaustive_matches(obj),
        Expr::Index { target, index } => {
            count_non_exhaustive_matches(target) + count_non_exhaustive_matches(index)
        }
        Expr::Cond {
            condition,
            then_expr,
            else_expr,
        } => {
            count_non_exhaustive_matches(condition)
                + count_non_exhaustive_matches(then_expr)
                + count_non_exhaustive_matches(else_expr)
        }
        Expr::Match {
            subject,
            cases,
            default,
        } => {
            usize::from(default.is_none())
                + count_non_exhaustive_matches(subject)
                + cases
                    .iter()
                    .map(|(pattern, result)| {
                        count_non_exhaustive_matches(pattern) + count_non_exhaustive_matches(result)
                    })
                    .sum::<usize>()
                + default
                    .as_deref()
                    .map(count_non_exhaustive_matches)
                    .unwrap_or(0)
        }
    }
}

/// Reserve variable spellings across the whole predicate before lowering
/// either side of a short-circuit operator or a conditional. Validation of
/// the generator itself remains in `lower_exists`.
fn reserved_variable_names(expr: &Expr) -> HashSet<String> {
    fn collect(expr: &Expr, names: &mut HashSet<String>) {
        match expr {
            Expr::Call { func, args } => {
                if func == "exists"
                    && let Some(Expr::Call {
                        args: generator_args,
                        ..
                    }) = args.first()
                {
                    for arg in generator_args {
                        if let Expr::Var(name) = arg
                            && term_named(name).is_none()
                        {
                            names.insert(name.clone());
                        }
                    }
                }
                for arg in args {
                    collect(arg, names);
                }
            }
            Expr::BinOp { left, right, .. } => {
                collect(left, names);
                collect(right, names);
            }
            Expr::UnaryOp { operand, .. } => collect(operand, names),
            Expr::FieldAccess { obj, .. } => collect(obj, names),
            Expr::Index { target, index } => {
                collect(target, names);
                collect(index, names);
            }
            Expr::Cond {
                condition,
                then_expr,
                else_expr,
            } => {
                collect(condition, names);
                collect(then_expr, names);
                collect(else_expr, names);
            }
            Expr::Match {
                subject,
                cases,
                default,
            } => {
                collect(subject, names);
                for (pattern, value) in cases {
                    collect(pattern, names);
                    collect(value, names);
                }
                if let Some(default) = default {
                    collect(default, names);
                }
            }
            Expr::LitInt(_)
            | Expr::LitFloat(_)
            | Expr::LitStr(_)
            | Expr::LitBool(_)
            | Expr::Var(_) => {}
        }
    }
    let mut names = HashSet::new();
    collect(expr, &mut names);
    names
}

/// Lower a derived relation's predicate. `parameter_names` are the names
/// rule lowering reads as parameters in the same module
/// ([`parameter_names`]); they take precedence over `derived_names`, as in
/// rule lowering.
pub(crate) fn lower_judgment_formula(
    formula: &str,
    derived_names: HashSet<String>,
    relation_predicates: HashSet<String>,
    parameter_names: HashSet<String>,
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
    let derived = derived_names
        .into_iter()
        .filter(|name| !parameter_names.contains(name))
        .collect();
    let ctx = LowerCtx {
        scalars: parameter_names,
        derived,
        relation_predicates,
        terms: true,
        reserved_variables: reserved_variable_names(&expr),
        variables: std::cell::RefCell::new(Vec::new()),
        relations: std::cell::RefCell::new(HashSet::new()),
    };
    lower_to_judgment(&expr, &ctx)
}

#[cfg(test)]
mod lexer_tests {
    use proptest::prelude::*;

    use super::{FormulaError, Lexer, TokType};

    /// Lex `src` and return the value of the one string token it holds.
    fn string_value(src: &str) -> Result<String, FormulaError> {
        let tokens = Lexer::new(src).tokenise()?;
        match tokens.as_slice() {
            [token, eof] if token.ty == TokType::String && eof.ty == TokType::Eof => {
                Ok(token.value.clone())
            }
            _ => panic!("{src:?} lexed to {tokens:?}, not one string token"),
        }
    }

    /// The character that a backslash escape of `c` stands for.
    fn unescape(c: char) -> char {
        match c {
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            other => other,
        }
    }

    #[test]
    fn every_escaped_character_decodes_whole() {
        // Every Unicode scalar value, so every UTF-8 width from one to four
        // bytes, in both quote styles. The trailing `é` checks that lexing
        // resumes after the whole escaped character, so a raw multibyte
        // character right after an escape comes through intact.
        for c in (0..=char::MAX as u32).filter_map(char::from_u32) {
            for quote in ['"', '\''] {
                let src = format!("{quote}\\{c}é{quote}");
                let value = string_value(&src).unwrap_or_else(|error| panic!("{src:?}: {error}"));
                assert_eq!(value, format!("{}é", unescape(c)), "{src:?}");
            }
        }
    }

    #[test]
    fn unexpected_character_is_reported_whole() {
        for (src, reported) in [("é", "'é'"), ("x €", "'€'"), ("𝄞", "'𝄞'")] {
            let error = Lexer::new(src)
                .tokenise()
                .expect_err("a character outside the grammar is an error");
            assert!(
                error
                    .to_string()
                    .contains(&format!("unexpected char {reported}")),
                "{src:?}: {error}"
            );
        }
    }

    /// Characters that reach every lexer branch, weighted toward those that
    /// open, close, or escape a string, and toward multibyte ones.
    fn lexer_char() -> impl Strategy<Value = char> {
        prop_oneof![
            3 => prop::sample::select(vec!['"', '\'', '\\', '#', '\n', '\r', '\t', ' ']),
            3 => prop::sample::select(vec!['é', 'ß', '€', '中', '𝄞', '😀', '\u{301}', '\u{feff}']),
            2 => prop::char::range('!', '~'),
            1 => any::<char>(),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(4096))]

        /// Every input lexes to tokens or to an ordinary parse error; none
        /// panics, and the cursor never stops inside a character. `slice`
        /// turns a split character into an error rather than a panic, so
        /// without the boundary check a cursor bug would pass unnoticed.
        #[test]
        fn tokenise_never_panics(chars in prop::collection::vec(lexer_char(), 0..64)) {
            let src: String = chars.into_iter().collect();
            if let Err(error) = Lexer::new(&src).tokenise() {
                prop_assert!(
                    !error.to_string().contains("not on a character boundary"),
                    "{:?}: {}",
                    src,
                    error
                );
            }
        }

        /// Any text, quoted with any mix of escaped and raw characters, lexes
        /// back to exactly that text.
        #[test]
        fn quoted_text_round_trips(
            parts in prop::collection::vec((lexer_char(), any::<bool>()), 0..48),
            single in any::<bool>(),
        ) {
            let quote = if single { '\'' } else { '"' };
            let mut literal = String::from(quote);
            let mut text = String::new();
            for (c, escape) in parts {
                text.push(c);
                match c {
                    '\n' if escape => literal.push_str("\\n"),
                    '\r' if escape => literal.push_str("\\r"),
                    '\t' if escape => literal.push_str("\\t"),
                    // Escaped, these letters name control characters.
                    'n' | 'r' | 't' => literal.push(c),
                    _ if escape || c == quote || c == '\\' => {
                        literal.push('\\');
                        literal.push(c);
                    }
                    _ => literal.push(c),
                }
            }
            literal.push(quote);
            match string_value(&literal) {
                Ok(value) => prop_assert_eq!(value, text, "{:?}", literal),
                Err(error) => {
                    return Err(TestCaseError::fail(format!("{literal:?}: {error}")));
                }
            }
        }
    }
}

#[cfg(test)]
mod builtin_tests {
    use std::collections::BTreeSet;

    use super::BUILTIN_FUNCTIONS;

    /// The names each `match func.as_str()` call lowering dispatches on: its
    /// arms at the match's own indentation, up to the catch-all arm.
    fn lowered_call_names() -> BTreeSet<String> {
        let source = include_str!("formula.rs");
        let mut names = BTreeSet::new();
        let mut matches = 0;
        let mut lines = source.lines();
        while let Some(line) = lines.next() {
            if line.trim() != "Expr::Call { func, args } => match func.as_str() {" {
                continue;
            }
            matches += 1;
            let arm_indent = line.len() - line.trim_start().len() + 4;
            for arm in lines.by_ref() {
                let indent = arm.len() - arm.trim_start().len();
                let arm = arm.trim();
                if indent != arm_indent || arm.is_empty() || arm.starts_with("//") {
                    continue;
                }
                let Some((pattern, _)) = arm.split_once("=>") else {
                    continue;
                };
                if !pattern.trim_start().starts_with('"') {
                    break;
                }
                for name in pattern.split('|') {
                    names.insert(name.trim().trim_matches('"').to_string());
                }
            }
        }
        assert_eq!(matches, 2, "scalar and judgment call lowering");
        names
    }

    #[test]
    fn builtin_function_names_are_the_lowered_call_names() {
        let listed = BUILTIN_FUNCTIONS
            .iter()
            .map(|name| name.to_string())
            .collect::<BTreeSet<_>>();
        assert_eq!(listed.len(), BUILTIN_FUNCTIONS.len(), "no duplicates");
        assert_eq!(listed, lowered_call_names());
    }
}
