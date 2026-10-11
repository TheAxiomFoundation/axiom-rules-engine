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

/// The deepest a RuleSpec formula may nest, in levels.
///
/// Every stage after parsing walks a formula's expression tree recursively:
/// lowering, conversion to the runtime program, each evaluator and the dense
/// compiler. A formula nested a few thousand levels deep, such as `((((1))))`
/// or `----1`, overflowed the stack. That aborts the process: it is not a
/// panic, so no host can catch it, and in wasm it is a trap. The parser
/// refuses a formula deeper than this before it builds a tree that deep, so
/// no later stage recurses further.
///
/// A formula's depth counts the levels of the tree lowering builds from it;
/// lowering adds at most two more. Each operator, call, index, field access
/// and `if` is a level, so a flat chain like `a + b + c` nests: it is
/// `(a + b) + c`. An `elif` or a `match` arm is a level too, because lowering
/// nests one `if` per branch. The parser also counts the expressions it is
/// inside while it parses, so parentheses, which build no level, cannot nest
/// past the bound either.
///
/// The bound equals the depth at which the evaluators defer a rule reference
/// in a release build ([`crate::depth`]). A chain of rules defers there, but
/// a formula cannot defer inside itself, so the bound caps what one formula
/// adds to a stack. Stack use is linear in depth. Measured in a release build
/// over the ways of nesting in `tests/formula_depth.rs`, a formula at the
/// bound took at most 0.4 MiB to parse, lower, compile and evaluate in
/// explain and fast, and 0.6 MiB in dense, so every mode fits a 1 MiB thread.
/// A debug build took up to 5.8 MiB.
///
/// The deepest formula in the RuleSpec corpora on 2026-10-10 needs 76 levels:
/// a generated tariff rule whose `elif` chain branches on the country of
/// origin. A formula that needs more than the bound splits into named rules:
/// a reference to a rule is one level, however deep that rule is, and a chain
/// of rules can be any length.
pub const MAX_FORMULA_DEPTH: usize = 128;

/// A parsed expression and its depth (see [`MAX_FORMULA_DEPTH`]).
struct Parsed {
    expr: Expr,
    depth: usize,
}

impl Parsed {
    fn leaf(expr: Expr) -> Self {
        Self { expr, depth: 1 }
    }
}

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    /// Expressions the parser is inside on its current path (see
    /// [`MAX_FORMULA_DEPTH`]).
    nesting: usize,
    /// The deepest a formula may nest: [`MAX_FORMULA_DEPTH`], or another
    /// bound in the unit tests.
    max_depth: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            pos: 0,
            nesting: 0,
            max_depth: MAX_FORMULA_DEPTH,
        }
    }

    /// A parser that bounds formulas at `max_depth` levels instead.
    #[cfg(test)]
    fn with_max_depth(tokens: Vec<Token>, max_depth: usize) -> Self {
        Self {
            max_depth,
            ..Self::new(tokens)
        }
    }

    fn too_deep(&self, at: &Token) -> FormulaError {
        FormulaError::parse(
            at.line,
            at.col,
            format!(
                "formula nests more than {} levels deep; split it into named rules",
                self.max_depth
            ),
        )
    }

    /// `depth`, if it is within the bound; otherwise the depth error, located
    /// at `at`.
    fn within_bound(&self, depth: usize, at: &Token) -> Result<usize, FormulaError> {
        if depth > self.max_depth {
            return Err(self.too_deep(at));
        }
        Ok(depth)
    }

    /// Parse with `parse` one expression deeper than the caller, refusing to
    /// nest past the bound before recursing.
    fn nested(
        &mut self,
        parse: impl FnOnce(&mut Self) -> Result<Parsed, FormulaError>,
    ) -> Result<Parsed, FormulaError> {
        if self.nesting >= self.max_depth {
            return Err(self.too_deep(self.peek(0)));
        }
        self.nesting += 1;
        let parsed = parse(self);
        self.nesting -= 1;
        parsed
    }

    /// `left op right`, if it is within the bound.
    fn binary(
        &self,
        op: BinOpKind,
        left: Parsed,
        right: Parsed,
        at: &Token,
    ) -> Result<Parsed, FormulaError> {
        let depth = self.within_bound(1 + left.depth.max(right.depth), at)?;
        Ok(Parsed {
            expr: Expr::BinOp {
                op,
                left: Box::new(left.expr),
                right: Box::new(right.expr),
            },
            depth,
        })
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

    /// Parse a whole expression: a formula, or one dated value of a
    /// declaration.
    fn parse_expr(&mut self) -> Result<Expr, FormulaError> {
        Ok(self.parse_nested_expr()?.expr)
    }

    /// Parse an expression inside the one being parsed: a parenthesized
    /// group, an argument, an index or a branch.
    fn parse_nested_expr(&mut self) -> Result<Parsed, FormulaError> {
        self.nested(|parser| {
            // Let-bindings: IDENT ASSIGN value body
            if parser.peek(0).ty == TokType::Ident && parser.peek(1).ty == TokType::Assign {
                // Let-bindings aren't lowered by our engine yet. Inline the value
                // at the use site manually; until then, this is an error if hit.
                let tok = parser.peek(0).clone();
                return Err(FormulaError::parse(
                    tok.line,
                    tok.col,
                    "let-bindings are not supported in RuleSpec formulas yet".to_string(),
                ));
            }
            if parser.at(&[TokType::Match]) {
                return parser.parse_match();
            }
            if parser.at(&[TokType::If]) {
                return parser.parse_cond();
            }
            parser.parse_or()
        })
    }

    fn parse_match(&mut self) -> Result<Parsed, FormulaError> {
        let keyword = self.consume(TokType::Match)?;
        let subject = self.parse_or()?;
        self.consume(TokType::Colon)?;
        let mut cases = Vec::new();
        let mut default: Option<Parsed> = None;
        // Lowering nests one `if` per arm, each comparing the subject with
        // the arm's pattern: arm `i` (from 1) puts its result `i` levels down
        // and its comparison one level further. `lowered` is the deepest of
        // those so far.
        let mut lowered = 0;
        let mut result_depth = 0;
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
            let arm = self.peek(0).clone();
            let pat = self.parse_primary()?;
            self.consume(TokType::Arrow)?;
            let res = self.parse_nested_expr()?;
            result_depth = result_depth.max(res.depth);
            if matches!(&pat.expr, Expr::Var(name) if name == "_") {
                default = Some(res);
            } else {
                let level = cases.len() + 1;
                lowered = lowered.max(level + res.depth.max(1 + subject.depth.max(pat.depth)));
                self.within_bound(lowered, &arm)?;
                cases.push((pat.expr, res.expr));
            }
        }
        // After the last arm comes the wildcard's result. Without a wildcard
        // it is the node that reports an uncovered subject, which holds the
        // subject and the patterns no deeper than the last arm's comparison.
        let fallback = default.as_ref().map_or(0, |default| default.depth);
        // A match with no arms lowers to its wildcard's result alone, but the
        // tree parsed here still holds the subject and that result one level
        // down.
        let parsed = 1 + subject.depth.max(result_depth);
        let depth = self.within_bound(lowered.max(cases.len() + fallback).max(parsed), &keyword)?;
        Ok(Parsed {
            expr: Expr::Match {
                subject: Box::new(subject.expr),
                cases,
                default: default.map(|default| Box::new(default.expr)),
            },
            depth,
        })
    }

    fn parse_cond(&mut self) -> Result<Parsed, FormulaError> {
        // `if c1: t1 elif c2: t2 else: e` is `if c1: t1 else: (if c2: t2
        // else: e)`, so branch `i` (from 1) sits `i` levels down. The chain
        // is read in a loop and nested afterwards, so its length costs no
        // stack.
        let mut keyword = self.consume(TokType::If)?;
        let mut branches = Vec::new();
        let mut depth = 0;
        loop {
            let condition = self.parse_or()?;
            self.consume(TokType::Colon)?;
            let then_expr = self.parse_nested_expr()?;
            let level = branches.len() + 1;
            depth = depth.max(level + condition.depth.max(then_expr.depth));
            self.within_bound(depth, &keyword)?;
            branches.push((condition.expr, then_expr.expr));
            match self.match_one(&[TokType::Elif]) {
                Some(elif) => keyword = elif,
                None => break,
            }
        }
        let else_keyword = self.consume(TokType::Else)?;
        self.consume(TokType::Colon)?;
        let else_expr = self.parse_nested_expr()?;
        let depth =
            self.within_bound(depth.max(branches.len() + else_expr.depth), &else_keyword)?;
        let mut expr = else_expr.expr;
        for (condition, then_expr) in branches.into_iter().rev() {
            expr = Expr::Cond {
                condition: Box::new(condition),
                then_expr: Box::new(then_expr),
                else_expr: Box::new(expr),
            };
        }
        Ok(Parsed { expr, depth })
    }

    fn parse_or(&mut self) -> Result<Parsed, FormulaError> {
        let mut left = self.parse_and()?;
        while let Some(or) = self.match_one(&[TokType::Or]) {
            let right = self.parse_and()?;
            left = self.binary(BinOpKind::Or, left, right, &or)?;
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Parsed, FormulaError> {
        let mut left = self.parse_cmp()?;
        while let Some(and) = self.match_one(&[TokType::And]) {
            let right = self.parse_cmp()?;
            left = self.binary(BinOpKind::And, left, right, &and)?;
        }
        Ok(left)
    }

    fn parse_cmp(&mut self) -> Result<Parsed, FormulaError> {
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
            let at = self.peek(0).clone();
            self.pos += 1;
            let right = self.parse_add()?;
            return self.binary(op, left, right, &at);
        }
        Ok(left)
    }

    fn parse_add(&mut self) -> Result<Parsed, FormulaError> {
        let mut left = self.parse_mul()?;
        loop {
            let op = match self.peek(0).ty {
                TokType::Plus => BinOpKind::Add,
                TokType::Minus => BinOpKind::Sub,
                _ => break,
            };
            let at = self.peek(0).clone();
            self.pos += 1;
            let right = self.parse_mul()?;
            left = self.binary(op, left, right, &at)?;
        }
        Ok(left)
    }

    fn parse_mul(&mut self) -> Result<Parsed, FormulaError> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek(0).ty {
                TokType::Star => BinOpKind::Mul,
                TokType::Slash => BinOpKind::Div,
                _ => break,
            };
            let at = self.peek(0).clone();
            self.pos += 1;
            let right = self.parse_unary()?;
            left = self.binary(op, left, right, &at)?;
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Parsed, FormulaError> {
        if let Some(minus) = self.match_one(&[TokType::Minus]) {
            return self.parse_operand(UnaryOpKind::Neg, &minus);
        }
        if let Some(not) = self.match_one(&[TokType::Not]) {
            return self.parse_operand(UnaryOpKind::Not, &not);
        }
        self.parse_postfix()
    }

    /// The operand of prefix operator `op`, which was read at `at`.
    fn parse_operand(&mut self, op: UnaryOpKind, at: &Token) -> Result<Parsed, FormulaError> {
        let operand = self.nested(Self::parse_unary)?;
        let depth = self.within_bound(1 + operand.depth, at)?;
        Ok(Parsed {
            expr: Expr::UnaryOp {
                op,
                operand: Box::new(operand.expr),
            },
            depth,
        })
    }

    fn parse_postfix(&mut self) -> Result<Parsed, FormulaError> {
        let mut expr = self.parse_primary()?;
        loop {
            if self.at(&[TokType::LParen]) {
                // Call — expr must be a Var
                let func = match &expr.expr {
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
                let open = self.consume(TokType::LParen)?;
                let mut args = Vec::new();
                let mut depth = 0;
                if !self.at(&[TokType::RParen]) {
                    loop {
                        let arg = self.parse_nested_expr()?;
                        depth = depth.max(arg.depth);
                        args.push(arg.expr);
                        if self.match_one(&[TokType::Comma]).is_none() {
                            break;
                        }
                    }
                }
                self.consume(TokType::RParen)?;
                expr = Parsed {
                    expr: Expr::Call { func, args },
                    depth: self.within_bound(1 + depth, &open)?,
                };
            } else if self.at(&[TokType::Dot]) {
                let dot = self.consume(TokType::Dot)?;
                let field = self.consume(TokType::Ident)?.value;
                expr = Parsed {
                    depth: self.within_bound(1 + expr.depth, &dot)?,
                    expr: Expr::FieldAccess {
                        obj: Box::new(expr.expr),
                        field,
                    },
                };
            } else if self.at(&[TokType::LBracket]) {
                let open = self.consume(TokType::LBracket)?;
                let index = self.parse_nested_expr()?;
                self.consume(TokType::RBracket)?;
                expr = Parsed {
                    depth: self.within_bound(1 + expr.depth.max(index.depth), &open)?,
                    expr: Expr::Index {
                        target: Box::new(expr.expr),
                        index: Box::new(index.expr),
                    },
                };
            } else {
                break;
            }
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Parsed, FormulaError> {
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
                Ok(Parsed::leaf(Expr::LitInt(v)))
            }
            TokType::Float => {
                self.pos += 1;
                let v = Decimal::from_str(&tok.value.replace('_', ""))
                    .map_err(|e| FormulaError::parse(tok.line, tok.col, e.to_string()))?;
                Ok(Parsed::leaf(Expr::LitFloat(v)))
            }
            TokType::String => {
                self.pos += 1;
                Ok(Parsed::leaf(Expr::LitStr(tok.value)))
            }
            TokType::True => {
                self.pos += 1;
                Ok(Parsed::leaf(Expr::LitBool(true)))
            }
            TokType::False => {
                self.pos += 1;
                Ok(Parsed::leaf(Expr::LitBool(false)))
            }
            TokType::Ident | TokType::Path => {
                self.pos += 1;
                Ok(Parsed::leaf(Expr::Var(tok.value)))
            }
            TokType::LParen => {
                self.consume(TokType::LParen)?;
                let e = self.parse_nested_expr()?;
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
                check_lowered_size(expr, &ctx, *dtype == DTypeSpec::Judgment, &v.path)?;
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

/// Counts what lowering a formula builds, counted as
/// `ScalarExprSpec::runtime_size` counts it, without building anything, so a
/// formula whose lowering would be huge is refused before it is built (see
/// [`check_lowered_size`]). [`Self::scalar`] and [`Self::judgment`] return the
/// size of the expression [`lower_to_scalar`] and [`lower_to_judgment`] build:
/// a `match` subject counts once per arm it is copied into, and an
/// `exactly_one` counts as what it expands to. The one thing lowering builds
/// and does not keep, the subject of a `match` with only `_`, which it lowers
/// to check and then drops, adds to [`Self::dropped`]. Every count saturates;
/// a shape the lowering refuses counts as nothing. It recurses as deep as the
/// formula, which the parser bounds.
struct LoweringSize<'a> {
    ctx: &'a LowerCtx,
    dropped: u64,
}

impl<'a> LoweringSize<'a> {
    fn new(ctx: &'a LowerCtx) -> Self {
        Self { ctx, dropped: 0 }
    }

    /// The expression `e` lowers to in scalar position.
    fn scalar(&mut self, e: &Expr) -> u64 {
        let one_plus = |size: u64| 1u64.saturating_add(size);
        match e {
            Expr::LitInt(_) | Expr::LitFloat(_) | Expr::LitBool(_) | Expr::LitStr(_) => 1,
            Expr::Var(name) => match name.as_str() {
                "period_start" | "period_end" => 1,
                // A parameter reads its index-0 value: a lookup and its literal.
                _ if self.ctx.scalars.contains(name) => 2,
                _ => 1,
            },
            Expr::Index { index, .. } => one_plus(self.scalar(index)),
            Expr::BinOp { left, right, .. } => {
                let left = self.scalar(left);
                one_plus(left.saturating_add(self.scalar(right)))
            }
            // `0 - operand`.
            Expr::UnaryOp { operand, .. } => 2u64.saturating_add(self.scalar(operand)),
            Expr::Call { func, args } => match func.as_str() {
                "max"
                | "min"
                | "ceil"
                | "floor"
                | "days_between"
                | "date_add_days"
                | "date_add_months"
                | "date_add_years"
                | "sum_over_periods"
                | "max_over_periods"
                | "count_over_periods"
                | "sum_top_n_over_periods" => one_plus(self.scalars(args)),
                "len" | "sum" => 1,
                // The related-entity count or sum, and its `where` clause: a
                // rule that holds, or an input compared with `true`.
                "count_where" | "sum_where" => {
                    let predicate = args.last().and_then(|predicate| match predicate {
                        Expr::Var(name) => Some(name),
                        _ => None,
                    });
                    if predicate.is_some_and(|name| self.ctx.derived.contains(name)) {
                        2
                    } else {
                        4
                    }
                }
                _ => 0,
            },
            Expr::Cond {
                condition,
                then_expr,
                else_expr,
            } => {
                let condition = self.judgment(condition);
                let then_expr = self.scalar(then_expr);
                one_plus(
                    condition
                        .saturating_add(then_expr)
                        .saturating_add(self.scalar(else_expr)),
                )
            }
            // Each arm is an `if` comparing a copy of the subject with the
            // arm's pattern. After the last arm comes the wildcard's result
            // or, without one, the node holding one more copy of the subject
            // and every pattern. With no arm and a wildcard, the subject is
            // lowered and dropped.
            Expr::Match {
                subject,
                cases,
                default,
            } => {
                let subject = self.scalar(subject);
                let fallback = match default {
                    Some(default) => self.scalar(default),
                    None => cases.iter().fold(one_plus(subject), |total, (pattern, _)| {
                        total.saturating_add(self.scalar(pattern))
                    }),
                };
                if cases.is_empty() {
                    self.dropped = self.dropped.saturating_add(subject);
                }
                cases.iter().fold(fallback, |total, (pattern, result)| {
                    let pattern = self.scalar(pattern);
                    total
                        .saturating_add(2)
                        .saturating_add(subject)
                        .saturating_add(pattern)
                        .saturating_add(self.scalar(result))
                })
            }
            Expr::FieldAccess { .. } => 0,
        }
    }

    fn scalars(&mut self, items: &[Expr]) -> u64 {
        items
            .iter()
            .fold(0u64, |total, item| total.saturating_add(self.scalar(item)))
    }

    /// The expression `e` lowers to in judgment position.
    fn judgment(&mut self, e: &Expr) -> u64 {
        let one_plus = |size: u64| 1u64.saturating_add(size);
        match e {
            // `literal == true`.
            Expr::LitBool(_) => 3,
            Expr::Var(name)
                if self.ctx.derived.contains(name)
                    || self.ctx.relation_predicates.contains(name) =>
            {
                1
            }
            // `input == true`.
            Expr::Var(_) => 3,
            Expr::BinOp { op, left, right } => match op {
                BinOpKind::And | BinOpKind::Or => {
                    let left = self.judgment(left);
                    one_plus(left.saturating_add(self.judgment(right)))
                }
                _ => {
                    let left = self.scalar(left);
                    one_plus(left.saturating_add(self.scalar(right)))
                }
            },
            Expr::UnaryOp { operand, .. } => one_plus(self.judgment(operand)),
            Expr::Call { func, args } if func == "exactly_one" => {
                let operands = args
                    .iter()
                    .fold(0u64, |total, arg| total.saturating_add(self.judgment(arg)));
                crate::spec::exactly_one_runtime_size(
                    u64::try_from(args.len()).unwrap_or(u64::MAX),
                    operands,
                )
            }
            _ => 0,
        }
    }
}

/// Refuse the formula of `name`, in judgment position or not, if lowering it
/// would build more than [`crate::spec::MAX_EXPRESSION_SIZE`] runtime nodes,
/// those it drops included. Checked before lowering, which would otherwise
/// build them first.
fn check_lowered_size(
    expr: &Expr,
    ctx: &LowerCtx,
    judgment: bool,
    name: &str,
) -> Result<(), FormulaError> {
    let mut size = LoweringSize::new(ctx);
    let kept = if judgment {
        size.judgment(expr)
    } else {
        size.scalar(expr)
    };
    if kept.saturating_add(size.dropped) > crate::spec::MAX_EXPRESSION_SIZE {
        return Err(FormulaError::lower(format!(
            "the formula of `{name}` expands to more than {} expression nodes; split it into named rules",
            crate::spec::MAX_EXPRESSION_SIZE
        )));
    }
    Ok(())
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
        Expr::Call { func, args } if args.is_empty() && matches!(func.as_str(), "max" | "min") => {
            return Err(FormulaError::lower(format!(
                "{func}() requires at least one operand"
            )));
        }
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
            // A copy of the subject for each arm's comparison, and one for the
            // fallback of a match without `_`. The last copy taken is the
            // subject itself, so lowering builds no copy it does not keep. A
            // match with only `_` takes none: its subject is lowered to check
            // it and dropped (see `LoweringSize`).
            let subject = lower_to_scalar(subject, ctx)?;
            let mut copies =
                std::iter::repeat_n(subject, cases.len() + usize::from(default.is_none()));
            let mut expr = match default {
                Some(default) => lower_to_scalar(default, ctx)?,
                None => ScalarExprSpec::NoMatch {
                    subject: Box::new(copies.next().expect("a copy for the fallback")),
                    patterns: cases
                        .iter()
                        .map(|(pattern, _)| lower_to_scalar(pattern, ctx))
                        .collect::<Result<_, _>>()?,
                },
            };
            for (pat, res) in cases.iter().rev() {
                expr = ScalarExprSpec::If {
                    condition: Box::new(JudgmentExprSpec::Comparison {
                        left: Box::new(copies.next().expect("a copy for each arm")),
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

pub(crate) fn lower_judgment_formula(
    name: &str,
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
    check_lowered_size(&expr, &ctx, true, name)?;
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
mod depth_tests {
    //! The parser's bound on formula depth (see [`MAX_FORMULA_DEPTH`]).
    //!
    //! The parser counts a formula's depth while it builds the tree: from the
    //! inside out, and arm by arm through a `match` or an `elif` chain. These
    //! tests check that count against an independent one. A generated formula
    //! has a shape, its depth is defined on that shape from the outside in,
    //! and the parser must accept the formula at exactly the bounds the
    //! definition allows and build the same tree at every bound that accepts
    //! it. They also check the depth against what the bound is for: the tree
    //! lowering builds is never more than two levels deeper.

    use super::*;

    /// Room for a debug build to parse to the bound; release needs far less
    /// (`tests/formula_depth.rs` measures the release stack).
    const STACK: usize = 16 * 1024 * 1024;

    fn on_stack<T: Send>(f: impl FnOnce() -> T + Send) -> T {
        std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(STACK)
                .spawn_scoped(scope, f)
                .expect("spawn test thread")
                .join()
                .expect("test thread")
        })
    }

    fn parse_at(formula: &str, max_depth: usize) -> Result<Expr, FormulaError> {
        let tokens = Lexer::new(formula).tokenise()?;
        let mut parser = Parser::with_max_depth(tokens, max_depth);
        let expr = parser.parse_expr()?;
        assert_eq!(parser.nesting, 0, "the parser left `{formula}` nested");
        assert_eq!(
            parser.peek(0).ty,
            TokType::Eof,
            "the parser stopped early in `{formula}`"
        );
        Ok(expr)
    }

    fn too_deep(result: &Result<Expr, FormulaError>, max_depth: usize) -> bool {
        matches!(
            result,
            Err(FormulaError::Parse { message, .. })
                if *message == format!(
                    "formula nests more than {max_depth} levels deep; split it into named rules"
                )
        )
    }

    /// The smallest bound that accepts `formula`.
    fn needed(formula: &str) -> usize {
        (1..=4 * MAX_FORMULA_DEPTH)
            .find(|bound| parse_at(formula, *bound).is_ok())
            .unwrap_or_else(|| panic!("no bound accepts `{formula}`"))
    }

    /// A generated formula as it is written: parentheses and `elif` chains
    /// are part of the shape.
    #[derive(Clone, Debug)]
    enum Shape {
        Int(i64),
        Var(&'static str),
        Bool(bool),
        Unary(UnaryOpKind, Box<Shape>),
        Binary(BinOpKind, Box<Shape>, Box<Shape>),
        Call(&'static str, Vec<Shape>),
        Index(Box<Shape>, Box<Shape>),
        Field(Box<Shape>, &'static str),
        Group(Box<Shape>),
        /// `if c: t elif c: t else: e`
        Cond(Vec<(Shape, Shape)>, Box<Shape>),
        /// `match s: p => r p => r _ => d`
        Match(Box<Shape>, Vec<(i64, Shape)>, Option<Box<Shape>>),
    }

    /// How tightly a shape binds: the grammar rule that parses it.
    fn binding(shape: &Shape) -> u8 {
        match shape {
            Shape::Cond(..) | Shape::Match(..) => 0,
            Shape::Binary(op, ..) => binary_binding(op),
            Shape::Unary(..) => 6,
            Shape::Call(..) | Shape::Index(..) | Shape::Field(..) => 7,
            Shape::Int(_) | Shape::Var(_) | Shape::Bool(_) | Shape::Group(_) => 8,
        }
    }

    fn binary_binding(op: &BinOpKind) -> u8 {
        match op {
            BinOpKind::Or => 1,
            BinOpKind::And => 2,
            BinOpKind::Lt
            | BinOpKind::Gt
            | BinOpKind::Le
            | BinOpKind::Ge
            | BinOpKind::Eq
            | BinOpKind::Ne => 3,
            BinOpKind::Add | BinOpKind::Sub => 4,
            BinOpKind::Mul | BinOpKind::Div => 5,
        }
    }

    /// `shape`, in parentheses unless it binds at least as tightly as `binding`.
    fn at_least(shape: Shape, required: u8) -> Shape {
        if binding(&shape) >= required {
            shape
        } else {
            Shape::Group(Box::new(shape))
        }
    }

    /// `left op right` with the parentheses the grammar needs. `or`, `and`,
    /// `+ -` and `* /` chain to the left; a comparison does not chain.
    fn binary(op: BinOpKind, left: Shape, right: Shape) -> Shape {
        let level = binary_binding(&op);
        let chains = level != 3;
        Shape::Binary(
            op,
            Box::new(at_least(left, if chains { level } else { level + 1 })),
            Box::new(at_least(right, level + 1)),
        )
    }

    fn symbol(op: &BinOpKind) -> &'static str {
        match op {
            BinOpKind::Add => "+",
            BinOpKind::Sub => "-",
            BinOpKind::Mul => "*",
            BinOpKind::Div => "/",
            BinOpKind::Lt => "<",
            BinOpKind::Gt => ">",
            BinOpKind::Le => "<=",
            BinOpKind::Ge => ">=",
            BinOpKind::Eq => "==",
            BinOpKind::Ne => "!=",
            BinOpKind::And => "and",
            BinOpKind::Or => "or",
        }
    }

    fn render(shape: &Shape) -> String {
        match shape {
            Shape::Int(value) => value.to_string(),
            Shape::Var(name) => name.to_string(),
            Shape::Bool(value) => value.to_string(),
            Shape::Unary(UnaryOpKind::Neg, operand) => format!("-{}", render(operand)),
            Shape::Unary(UnaryOpKind::Not, operand) => format!("not {}", render(operand)),
            Shape::Binary(op, left, right) => {
                format!("{} {} {}", render(left), symbol(op), render(right))
            }
            Shape::Call(name, args) => format!(
                "{name}({})",
                args.iter().map(render).collect::<Vec<_>>().join(", ")
            ),
            Shape::Index(target, index) => format!("{}[{}]", render(target), render(index)),
            Shape::Field(object, field) => format!("{}.{field}", render(object)),
            Shape::Group(inner) => format!("({})", render(inner)),
            Shape::Cond(branches, else_expr) => {
                let mut text = String::new();
                for (index, (condition, then_expr)) in branches.iter().enumerate() {
                    let keyword = if index == 0 { "if" } else { " elif" };
                    text.push_str(&format!(
                        "{keyword} {}: {}",
                        render(condition),
                        render(then_expr)
                    ));
                }
                text.push_str(&format!(" else: {}", render(else_expr)));
                text
            }
            Shape::Match(subject, arms, default) => {
                let mut text = format!("match {}:", render(subject));
                for (pattern, result) in arms {
                    text.push_str(&format!(" {pattern} => {}", render(result)));
                }
                if let Some(default) = default {
                    text.push_str(&format!(" _ => {}", render(default)));
                }
                text
            }
        }
    }

    /// The tree the parser builds for `shape`.
    fn tree(shape: &Shape) -> Expr {
        match shape {
            Shape::Int(value) => Expr::LitInt(*value),
            Shape::Var(name) => Expr::Var(name.to_string()),
            Shape::Bool(value) => Expr::LitBool(*value),
            Shape::Unary(op, operand) => Expr::UnaryOp {
                op: op.clone(),
                operand: Box::new(tree(operand)),
            },
            Shape::Binary(op, left, right) => Expr::BinOp {
                op: op.clone(),
                left: Box::new(tree(left)),
                right: Box::new(tree(right)),
            },
            Shape::Call(name, args) => Expr::Call {
                func: name.to_string(),
                args: args.iter().map(tree).collect(),
            },
            Shape::Index(target, index) => Expr::Index {
                target: Box::new(tree(target)),
                index: Box::new(tree(index)),
            },
            Shape::Field(object, field) => Expr::FieldAccess {
                obj: Box::new(tree(object)),
                field: field.to_string(),
            },
            Shape::Group(inner) => tree(inner),
            Shape::Cond(branches, else_expr) => {
                branches
                    .iter()
                    .rev()
                    .fold(tree(else_expr), |else_expr, (condition, then_expr)| {
                        Expr::Cond {
                            condition: Box::new(tree(condition)),
                            then_expr: Box::new(tree(then_expr)),
                            else_expr: Box::new(else_expr),
                        }
                    })
            }
            Shape::Match(subject, arms, default) => Expr::Match {
                subject: Box::new(tree(subject)),
                cases: arms
                    .iter()
                    .map(|(pattern, result)| (Expr::LitInt(*pattern), tree(result)))
                    .collect(),
                default: default.as_ref().map(|default| Box::new(tree(default))),
            },
        }
    }

    /// The depth of `shape`, from the outside in: the definition the parser's
    /// running count must agree with.
    fn depth(shape: &Shape) -> usize {
        match shape {
            Shape::Int(_) | Shape::Var(_) | Shape::Bool(_) => 1,
            Shape::Unary(_, operand) => 1 + depth(operand),
            Shape::Binary(_, left, right) => 1 + depth(left).max(depth(right)),
            Shape::Call(_, args) => 1 + args.iter().map(depth).max().unwrap_or(0),
            Shape::Index(target, index) => 1 + depth(target).max(depth(index)),
            Shape::Field(object, _) => 1 + depth(object),
            Shape::Group(inner) => depth(inner),
            // Each branch is an `if` whose `else` is the rest of the chain.
            Shape::Cond(branches, else_expr) => branches
                .iter()
                .rev()
                .fold(depth(else_expr), |rest, (condition, then_expr)| {
                    1 + depth(condition).max(depth(then_expr)).max(rest)
                }),
            Shape::Match(subject, arms, default) => {
                let subject = depth(subject);
                // Lowering: each arm is an `if` comparing the subject with a
                // one-level pattern, whose `else` is the rest of the arms.
                // The arms end in the wildcard's result or, without one, in a
                // node holding the subject and the patterns.
                let comparison = 1 + subject.max(1);
                let end = match default {
                    Some(default) => depth(default),
                    None if arms.is_empty() => 1 + subject,
                    None => comparison,
                };
                let lowered = arms.iter().rev().fold(end, |rest, (_, result)| {
                    1 + comparison.max(depth(result)).max(rest)
                });
                // The parsed tree: the subject, patterns and results sit one
                // level below the match.
                let children = arms
                    .iter()
                    .map(|(_, result)| depth(result).max(1))
                    .chain(default.iter().map(|default| depth(default)))
                    .max()
                    .unwrap_or(0);
                lowered.max(1 + subject.max(children))
            }
        }
    }

    /// The most expressions the parser is inside at once while parsing
    /// `shape`, itself inside `open`: an operand of a prefix operator, a
    /// parenthesized group, an argument, an index, a branch's result and a
    /// match arm's result each open one.
    fn nesting(shape: &Shape, open: usize) -> usize {
        match shape {
            Shape::Int(_) | Shape::Var(_) | Shape::Bool(_) => open,
            Shape::Unary(_, operand) => nesting(operand, open + 1),
            Shape::Binary(_, left, right) => nesting(left, open).max(nesting(right, open)),
            Shape::Call(_, args) => args
                .iter()
                .map(|arg| nesting(arg, open + 1))
                .max()
                .unwrap_or(open),
            Shape::Index(target, index) => nesting(target, open).max(nesting(index, open + 1)),
            Shape::Field(object, _) => nesting(object, open),
            Shape::Group(inner) => nesting(inner, open + 1),
            Shape::Cond(branches, else_expr) => branches
                .iter()
                .map(|(condition, then_expr)| {
                    nesting(condition, open).max(nesting(then_expr, open + 1))
                })
                .max()
                .unwrap_or(open)
                .max(nesting(else_expr, open + 1)),
            Shape::Match(subject, arms, default) => arms
                .iter()
                .map(|(_, result)| nesting(result, open + 1))
                .chain(default.iter().map(|default| nesting(default, open + 1)))
                .max()
                .unwrap_or(open)
                .max(nesting(subject, open)),
        }
    }

    /// The bound `shape` needs: its depth, or the parser's nesting if
    /// parentheses make that greater. A whole formula is itself one
    /// expression the parser is inside.
    fn bound_needed(shape: &Shape) -> usize {
        depth(shape).max(nesting(shape, 1))
    }

    /// A deterministic generator, as in `tests/execution.rs`: no random
    /// dependency, and a failing seed reproduces.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, bound: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            usize::try_from((self.0 >> 33) % u64::try_from(bound).expect("bound fits"))
                .expect("value fits")
        }

        fn pick<T: Clone>(&mut self, choices: &[T]) -> T {
            choices[self.below(choices.len())].clone()
        }
    }

    fn leaf(rng: &mut Rng) -> Shape {
        match rng.below(3) {
            0 => Shape::Int(i64::try_from(rng.below(10)).expect("small literal")),
            1 => Shape::Var(rng.pick(&["a", "b", "c"])),
            _ => Shape::Bool(rng.below(2) == 0),
        }
    }

    const BINARY_OPS: [BinOpKind; 12] = [
        BinOpKind::Add,
        BinOpKind::Sub,
        BinOpKind::Mul,
        BinOpKind::Div,
        BinOpKind::Lt,
        BinOpKind::Gt,
        BinOpKind::Le,
        BinOpKind::Ge,
        BinOpKind::Eq,
        BinOpKind::Ne,
        BinOpKind::And,
        BinOpKind::Or,
    ];

    /// Something a `.field` or `[index]` can follow as written.
    fn postfix_target(shape: Shape) -> Shape {
        match shape {
            Shape::Var(_) | Shape::Call(..) | Shape::Index(..) | Shape::Field(..) => shape,
            Shape::Group(_) => shape,
            other => Shape::Group(Box::new(other)),
        }
    }

    /// Any formula the grammar accepts, about `size` levels deep at most
    /// along its deepest spine.
    fn generate(rng: &mut Rng, size: usize) -> Shape {
        if size == 0 {
            return leaf(rng);
        }
        let smaller = |rng: &mut Rng| {
            let size = rng.below(size);
            generate(rng, size / 2)
        };
        match rng.below(13) {
            0 => leaf(rng),
            1 | 2 => Shape::Unary(
                rng.pick(&[UnaryOpKind::Neg, UnaryOpKind::Not]),
                Box::new(at_least(generate(rng, size - 1), 6)),
            ),
            3 => {
                let op = rng.pick(&BINARY_OPS);
                binary(op, generate(rng, size - 1), smaller(rng))
            }
            4 => {
                let op = rng.pick(&BINARY_OPS);
                binary(op, smaller(rng), generate(rng, size - 1))
            }
            // A flat chain, which nests to the left without the parser
            // recursing.
            5 => {
                let links = 1 + rng.below(size);
                let ops = rng.pick(&[
                    [BinOpKind::Add, BinOpKind::Sub],
                    [BinOpKind::Mul, BinOpKind::Div],
                    [BinOpKind::And, BinOpKind::And],
                    [BinOpKind::Or, BinOpKind::Or],
                ]);
                let mut chain = generate(rng, size - links);
                for _ in 0..links {
                    let op = rng.pick(&ops);
                    chain = binary(op, chain, smaller(rng));
                }
                chain
            }
            6 => {
                let args = rng.below(4);
                let mut shapes = Vec::new();
                for arg in 0..args {
                    shapes.push(if arg == 0 {
                        generate(rng, size - 1)
                    } else {
                        smaller(rng)
                    });
                }
                Shape::Call(rng.pick(&["max", "min", "f"]), shapes)
            }
            7 => Shape::Index(
                Box::new(postfix_target(generate(rng, size - 1))),
                Box::new(smaller(rng)),
            ),
            8 => Shape::Index(
                Box::new(postfix_target(smaller(rng))),
                Box::new(generate(rng, size - 1)),
            ),
            9 => Shape::Field(
                Box::new(postfix_target(generate(rng, size - 1))),
                rng.pick(&["x", "y"]),
            ),
            10 => Shape::Group(Box::new(generate(rng, size - 1))),
            11 => {
                let count = 1 + rng.below(size);
                let mut branches = Vec::new();
                for _ in 0..count {
                    branches.push((at_least(smaller(rng), 1), smaller(rng)));
                }
                Shape::Cond(branches, Box::new(generate(rng, size - count)))
            }
            _ => {
                let count = rng.below(size + 1);
                let subject = at_least(smaller(rng), 1);
                let mut arms = Vec::new();
                for pattern in 0..count {
                    // In parentheses, so an arm's `match` cannot take the
                    // arms that follow it.
                    arms.push((
                        i64::try_from(pattern).expect("small pattern"),
                        at_least(smaller(rng), 1),
                    ));
                }
                let default =
                    (rng.below(2) == 0).then(|| Box::new(at_least(generate(rng, size - count), 1)));
                Shape::Match(Box::new(subject), arms, default)
            }
        }
    }

    /// The parser accepts a generated formula at exactly the bounds its shape
    /// allows, refuses it with the depth error below them, and builds the
    /// same tree whatever bound accepts it.
    #[test]
    fn the_parser_accepts_exactly_the_formulas_within_the_bound() {
        on_stack(|| {
            let mut deepest = 0;
            for seed in 0..4_000_u64 {
                let mut rng = Rng(seed.wrapping_add(0x9e37_79b9_7f4a_7c15));
                let size = 1 + usize::try_from(seed % 28).expect("small size");
                let shape = generate(&mut rng, size);
                let formula = render(&shape);
                let expected = format!("{:?}", tree(&shape));
                let need = bound_needed(&shape);
                deepest = deepest.max(need);
                let context = format!("seed {seed}: `{formula}` needs {need}");

                let parsed = parse_at(&formula, need)
                    .unwrap_or_else(|error| panic!("{context}: refused at its bound: {error}"));
                assert_eq!(format!("{parsed:?}"), expected, "{context}: tree");
                assert!(
                    too_deep(&parse_at(&formula, need - 1), need - 1) || need == 1,
                    "{context}: accepted one level below its bound"
                );
                // Any other bound: accepted with the same tree, or refused.
                let bound = 1 + rng.below(need + 4);
                let result = parse_at(&formula, bound);
                if bound >= need {
                    let parsed = result
                        .unwrap_or_else(|error| panic!("{context}: refused at {bound}: {error}"));
                    assert_eq!(
                        format!("{parsed:?}"),
                        expected,
                        "{context}: tree at {bound}"
                    );
                } else {
                    assert!(too_deep(&result, bound), "{context}: accepted at {bound}");
                }
            }
            assert!(
                deepest > 40,
                "the generator reached only {deepest} levels; it no longer exercises deep formulas"
            );
        });
    }

    /// A scalar formula lowering accepts: arithmetic, `max`/`min`, `if` and
    /// `match`, over inputs.
    fn generate_scalar(rng: &mut Rng, size: usize) -> Shape {
        if size == 0 {
            return match rng.below(2) {
                0 => Shape::Int(i64::try_from(rng.below(10)).expect("small literal")),
                _ => Shape::Var(rng.pick(&["a", "b", "c"])),
            };
        }
        let smaller = |rng: &mut Rng| {
            let size = rng.below(size);
            generate_scalar(rng, size / 2)
        };
        const ARITHMETIC: [BinOpKind; 4] = [
            BinOpKind::Add,
            BinOpKind::Sub,
            BinOpKind::Mul,
            BinOpKind::Div,
        ];
        match rng.below(8) {
            0 => Shape::Unary(
                UnaryOpKind::Neg,
                Box::new(at_least(generate_scalar(rng, size - 1), 6)),
            ),
            1 => {
                let op = rng.pick(&ARITHMETIC);
                binary(op, generate_scalar(rng, size - 1), smaller(rng))
            }
            2 => {
                let links = 1 + rng.below(size);
                let mut chain = generate_scalar(rng, size - links);
                for _ in 0..links {
                    let op = rng.pick(&[BinOpKind::Add, BinOpKind::Sub]);
                    chain = binary(op, chain, smaller(rng));
                }
                chain
            }
            3 => Shape::Call(
                rng.pick(&["max", "min"]),
                vec![generate_scalar(rng, size - 1), smaller(rng)],
            ),
            4 => Shape::Group(Box::new(generate_scalar(rng, size - 1))),
            5 | 6 => {
                let count = 1 + rng.below(size);
                let mut branches = Vec::new();
                for _ in 0..count {
                    let part = rng.below(size) / 2;
                    let condition = generate_judgment(rng, part);
                    branches.push((at_least(condition, 1), smaller(rng)));
                }
                Shape::Cond(branches, Box::new(generate_scalar(rng, size - count)))
            }
            _ => {
                let count = 1 + rng.below(size);
                let subject = at_least(smaller(rng), 1);
                let mut arms = Vec::new();
                for pattern in 0..count {
                    arms.push((
                        i64::try_from(pattern).expect("small pattern"),
                        at_least(smaller(rng), 1),
                    ));
                }
                let default = (rng.below(2) == 0)
                    .then(|| Box::new(at_least(generate_scalar(rng, size - count), 1)));
                Shape::Match(Box::new(subject), arms, default)
            }
        }
    }

    /// A judgment lowering accepts: comparisons, `and`, `or`, `not`, and
    /// bare booleans and inputs.
    fn generate_judgment(rng: &mut Rng, size: usize) -> Shape {
        if size == 0 {
            return match rng.below(2) {
                0 => Shape::Bool(rng.below(2) == 0),
                _ => Shape::Var(rng.pick(&["p", "q"])),
            };
        }
        match rng.below(4) {
            0 => Shape::Unary(
                UnaryOpKind::Not,
                Box::new(at_least(generate_judgment(rng, size - 1), 6)),
            ),
            1 => {
                let op = rng.pick(&[BinOpKind::Lt, BinOpKind::Ge, BinOpKind::Eq, BinOpKind::Ne]);
                let part = rng.below(size) / 2;
                let right = generate_scalar(rng, part);
                binary(op, generate_scalar(rng, size - 1), right)
            }
            _ => {
                let links = 1 + rng.below(size);
                let op = rng.pick(&[BinOpKind::And, BinOpKind::Or]);
                let mut chain = generate_judgment(rng, size - links);
                for _ in 0..links {
                    let part = rng.below(size) / 2;
                    let right = generate_judgment(rng, part);
                    chain = binary(op.clone(), chain, right);
                }
                chain
            }
        }
    }

    /// How deep a serialized expression nests, in objects: one per node of
    /// the lowered tree, plus one for a literal's value.
    fn object_depth(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::Object(fields) => {
                1 + fields.values().map(object_depth).max().unwrap_or(0)
            }
            serde_json::Value::Array(items) => items.iter().map(object_depth).max().unwrap_or(0),
            _ => 0,
        }
    }

    /// The bound is on the tree lowering builds, which is what every later
    /// stage walks. Lowering adds at most two levels to a formula's depth: a
    /// literal's value is an object inside its node, and a bare boolean
    /// lowers to a comparison of two literals. A `match` in particular adds
    /// none, though it lowers to one `if` per arm. So every compiled rule is
    /// within `spec::MAX_EXPRESSION_DEPTH`, counted in expression nodes, with
    /// a level to spare for a request pin; scalar formulas and judgments
    /// (conditions, and a rule or relation predicate of its own) both.
    #[test]
    fn lowering_builds_a_tree_no_deeper_than_the_formula_plus_two_levels() {
        on_stack(|| {
            let ctx = LowerCtx {
                scalars: HashSet::new(),
                derived: HashSet::new(),
                relation_predicates: HashSet::new(),
                relations: std::cell::RefCell::new(HashSet::new()),
            };
            let (mut deepest, mut tight) = (0, 0);
            for seed in 0..3_000_u64 {
                let mut rng = Rng(seed.wrapping_add(0x9e37_79b9_7f4a_7c15));
                let size = 1 + usize::try_from(seed % 24).expect("small size");
                let shape = generate_scalar(&mut rng, size);
                let formula = render(&shape);
                let parsed = parse_at(&formula, usize::MAX)
                    .unwrap_or_else(|error| panic!("seed {seed}: `{formula}`: {error}"));
                let lowered = lower_to_scalar(&parsed, &ctx)
                    .unwrap_or_else(|error| panic!("seed {seed}: `{formula}`: {error}"));
                let lowered_depth =
                    object_depth(&serde_json::to_value(&lowered).expect("expression serializes"));
                let formula_depth = depth(&shape);
                assert!(
                    lowered_depth <= formula_depth + 2,
                    "seed {seed}: `{formula}` is {formula_depth} levels deep but lowers to {lowered_depth}"
                );
                assert!(
                    !lowered.deeper_than(formula_depth + 2),
                    "seed {seed}: `{formula}` lowers to more than {formula_depth} + 2 expression nodes"
                );
                deepest = deepest.max(formula_depth);
                tight += usize::from(lowered_depth >= formula_depth);

                let shape = generate_judgment(&mut rng, size);
                let formula = render(&shape);
                let parsed = parse_at(&formula, usize::MAX)
                    .unwrap_or_else(|error| panic!("seed {seed}: `{formula}`: {error}"));
                let lowered = lower_to_judgment(&parsed, &ctx)
                    .unwrap_or_else(|error| panic!("seed {seed}: `{formula}`: {error}"));
                assert!(
                    !lowered.deeper_than(depth(&shape) + 2),
                    "seed {seed}: judgment `{formula}` lowers to more than {} + 2 expression nodes",
                    depth(&shape)
                );
            }
            assert!(deepest > 30, "the generator reached only {deepest} levels");
            // The count is not slack either: lowering reaches the formula's
            // depth for most formulas.
            assert!(
                tight > 2_000,
                "only {tight} of 3,000 formulas lower as deep as counted"
            );
        });
    }

    /// `shape` with the arms of some of its `match`es removed, leaving only
    /// `_`, so the lowering drops their subjects.
    fn without_some_arms(shape: Shape, rng: &mut Rng) -> Shape {
        let inner = |shape: Box<Shape>, rng: &mut Rng| Box::new(without_some_arms(*shape, rng));
        match shape {
            Shape::Int(_) | Shape::Var(_) | Shape::Bool(_) => shape,
            Shape::Unary(op, operand) => Shape::Unary(op, inner(operand, rng)),
            Shape::Binary(op, left, right) => {
                let left = inner(left, rng);
                Shape::Binary(op, left, inner(right, rng))
            }
            Shape::Call(func, args) => Shape::Call(
                func,
                args.into_iter()
                    .map(|arg| without_some_arms(arg, rng))
                    .collect(),
            ),
            Shape::Index(target, index) => {
                let target = inner(target, rng);
                Shape::Index(target, inner(index, rng))
            }
            Shape::Field(object, field) => Shape::Field(inner(object, rng), field),
            Shape::Group(group) => Shape::Group(inner(group, rng)),
            Shape::Cond(branches, else_expr) => {
                let branches = branches
                    .into_iter()
                    .map(|(condition, then_expr)| {
                        let condition = without_some_arms(condition, rng);
                        (condition, without_some_arms(then_expr, rng))
                    })
                    .collect();
                Shape::Cond(branches, inner(else_expr, rng))
            }
            Shape::Match(subject, arms, default) => {
                let subject = inner(subject, rng);
                let arms: Vec<_> = arms
                    .into_iter()
                    .map(|(pattern, result)| (pattern, without_some_arms(result, rng)))
                    .collect();
                let default = default.map(|default| inner(default, rng));
                let arms = if default.is_some() && rng.below(3) == 0 {
                    Vec::new()
                } else {
                    arms
                };
                Shape::Match(subject, arms, default)
            }
        }
    }

    /// The nodes lowering `e` builds and drops, found without
    /// `LoweringSize`: the subject of each `match` with only `_`, sized by
    /// lowering it. Every subexpression of the formulas it is used on is
    /// lowered once (their patterns are literals) and as an expression.
    fn dropped_by_lowering(e: &Expr, ctx: &LowerCtx) -> u64 {
        let children: Vec<&Expr> = match e {
            Expr::LitInt(_)
            | Expr::LitFloat(_)
            | Expr::LitStr(_)
            | Expr::LitBool(_)
            | Expr::Var(_) => Vec::new(),
            Expr::BinOp { left, right, .. } => vec![left, right],
            Expr::UnaryOp { operand, .. } => vec![operand],
            Expr::Call { args, .. } => args.iter().collect(),
            Expr::FieldAccess { obj, .. } => vec![obj],
            Expr::Index { target, index } => vec![target, index],
            Expr::Cond {
                condition,
                then_expr,
                else_expr,
            } => vec![condition, then_expr, else_expr],
            Expr::Match {
                subject,
                cases,
                default,
            } => std::iter::once(subject.as_ref())
                .chain(cases.iter().flat_map(|(pattern, result)| [pattern, result]))
                .chain(default.as_deref())
                .collect(),
        };
        let own = match e {
            Expr::Match {
                subject,
                cases,
                default: Some(_),
            } if cases.is_empty() => lower_to_scalar(subject, ctx)
                .expect("a dropped subject lowers")
                .runtime_size(),
            _ => 0,
        };
        children
            .into_iter()
            .fold(own, |total, child| total + dropped_by_lowering(child, ctx))
    }

    /// The pre-flight count is the lowered formula's runtime size, so the
    /// size bound refuses exactly the formulas whose lowering would pass it,
    /// and what it counts as dropped is what lowering drops. On generated
    /// formulas, with `exactly_one`, `match` subjects that are themselves
    /// `match`es and `match`es with only `_`, in contexts that make a name a
    /// parameter, a rule or a relation predicate. The formulas come from
    /// fixed seeds, and the largest lowers to under 100,000 nodes, so no
    /// count, right or wrong, can make this test build a large tree.
    #[test]
    fn the_lowering_count_is_the_lowered_runtime_size() {
        let names = |set: &[&str]| {
            set.iter()
                .map(|name| name.to_string())
                .collect::<HashSet<_>>()
        };
        let contexts = [
            (names(&[]), names(&[]), names(&[])),
            (names(&["a", "t"]), names(&["b", "q"]), names(&["p"])),
            (names(&["c"]), names(&["a"]), names(&["q"])),
        ];
        // Every lowering rule at least once.
        let fixed = [
            ("t[a + 1]", false),
            ("period_start", false),
            ("count_where(r, b) + count_where(r, z)", false),
            (
                "sum_where(r, f, b) + sum_where(r, f, z) + len(r) + sum(r.f)",
                false,
            ),
            (
                "ceil(a) + floor(b) + days_between(period_start, period_end)",
                false,
            ),
            ("date_add_days(period_start, 1)", false),
            ("date_add_months(period_start, a)", false),
            ("date_add_years(period_start, -b)", false),
            (
                "sum_over_periods(a) + max_over_periods(b) + count_over_periods(c)",
                false,
            ),
            ("sum_top_n_over_periods(a, 2)", false),
            ("match a: 1 => b 2 => c _ => 0", false),
            ("match a + 1: 1 => b 2 => c", false),
            ("match a + b: _ => c", false),
            (
                "match (match a + b: _ => c): 1 => (match c: _ => 1) 2 => a",
                false,
            ),
            ("(match (match a: 1 => 2 _ => 3): _ => b) > 1 or p", true),
            (
                "match (match a: 1 => 2 _ => 3): 2 => b 3 => (match c: 1 => 1 _ => 0) _ => 0",
                false,
            ),
            ("if p and not q: a else: b", false),
            ("exactly_one(p, q, a > 1, b == 2)", true),
            (
                "exactly_one(exactly_one(p, q), exactly_one(q, p, true))",
                true,
            ),
            ("true or p", true),
        ];
        let mut cases: Vec<(String, bool)> = fixed
            .iter()
            .map(|(formula, judgment)| (formula.to_string(), *judgment))
            .collect();
        for seed in 0..3_000_u64 {
            let mut rng = Rng(seed.wrapping_add(0x9e37_79b9_7f4a_7c15));
            let size = 1 + usize::try_from(seed % 16).expect("small size");
            let judgment = seed % 3 == 0;
            let shape = if judgment {
                let mut shape = generate_judgment(&mut rng, size);
                if rng.below(2) == 0 {
                    shape = without_some_arms(shape, &mut rng);
                }
                if rng.below(2) == 0 {
                    let count = 2 + rng.below(3);
                    let mut items = vec![shape];
                    for _ in 1..count {
                        let item_size = rng.below(size);
                        items.push(generate_judgment(&mut rng, item_size));
                    }
                    shape = Shape::Call("exactly_one", items);
                }
                shape
            } else {
                let mut shape = generate_scalar(&mut rng, size);
                if rng.below(2) == 0 {
                    shape = without_some_arms(shape, &mut rng);
                }
                if rng.below(3) == 0 {
                    // A `match` whose subject is the generated formula; with
                    // no arms, one that drops it.
                    let arms = (0..rng.below(5))
                        .map(|arm| (i64::try_from(arm).expect("small"), Shape::Var("c")))
                        .collect();
                    Shape::Match(
                        Box::new(at_least(shape, 1)),
                        arms,
                        Some(Box::new(Shape::Int(0))),
                    )
                } else {
                    shape
                }
            };
            cases.push((render(&shape), judgment));
        }
        let (mut checked, mut largest, mut dropping) = (0usize, 0u64, 0usize);
        let mut fixed_lowered = vec![false; fixed.len()];
        for (index, (formula, judgment)) in cases.iter().enumerate() {
            // Each fixed formula in every context; a generated one in one.
            let in_contexts: Vec<usize> = if index < fixed.len() {
                (0..contexts.len()).collect()
            } else {
                vec![index % contexts.len()]
            };
            for context in in_contexts {
                let (scalars, derived, relation_predicates) = contexts[context].clone();
                let ctx = LowerCtx {
                    scalars,
                    derived,
                    relation_predicates,
                    relations: std::cell::RefCell::new(HashSet::new()),
                };
                let parsed = parse_at(formula, MAX_FORMULA_DEPTH)
                    .unwrap_or_else(|error| panic!("`{formula}`: {error}"));
                let mut count = LoweringSize::new(&ctx);
                let counted = if *judgment {
                    count.judgment(&parsed)
                } else {
                    count.scalar(&parsed)
                };
                let lowered = if *judgment {
                    lower_to_judgment(&parsed, &ctx).map(|expr| expr.runtime_size())
                } else {
                    lower_to_scalar(&parsed, &ctx).map(|expr| expr.runtime_size())
                };
                if let Ok(size) = lowered {
                    assert_eq!(counted, size, "`{formula}` in context {context}");
                    assert_eq!(
                        count.dropped,
                        dropped_by_lowering(&parsed, &ctx),
                        "`{formula}` in context {context}: dropped"
                    );
                    checked += 1;
                    largest = largest.max(size);
                    dropping += usize::from(count.dropped > 0);
                    if index < fixed.len() {
                        fixed_lowered[index] = true;
                    }
                }
            }
        }
        for (index, lowered) in fixed_lowered.iter().enumerate() {
            assert!(lowered, "`{}` lowers in no context", fixed[index].0);
        }
        assert!(
            checked > 2_500 && largest > 1_000 && largest < 100_000 && dropping > 300,
            "{checked} checked, the largest {largest} nodes, {dropping} dropping a subject"
        );
    }

    /// A `match` with only `_` lowers its subject and drops it, so the
    /// subject counts toward the size bound: exactly, and before anything is
    /// built. `max` of `n` names is `n + 1` nodes, and the wildcard's `0` one.
    /// The review's formula, whose subject is forty `match`es each the subject
    /// of the next, keeps one node and drops `10 × 2^40 − 9`; only counted
    /// here, never lowered.
    #[test]
    fn a_dropped_subject_counts_toward_the_size_bound() {
        let ctx = LowerCtx {
            scalars: HashSet::new(),
            derived: HashSet::new(),
            relation_predicates: HashSet::new(),
            relations: std::cell::RefCell::new(HashSet::new()),
        };
        let max = crate::spec::MAX_EXPRESSION_SIZE;
        let dropping_max_of = |names: u64| {
            let names = usize::try_from(names).expect("fits");
            format!("match max({}): _ => 0", vec!["x"; names].join(", "))
        };
        let parsed = parse_at(&dropping_max_of(max - 2), MAX_FORMULA_DEPTH).expect("parses");
        let mut size = LoweringSize::new(&ctx);
        assert_eq!((size.scalar(&parsed), size.dropped), (1, max - 1));
        assert!(check_lowered_size(&parsed, &ctx, false, "f").is_ok());
        let parsed = parse_at(&dropping_max_of(max - 1), MAX_FORMULA_DEPTH).expect("parses");
        assert!(check_lowered_size(&parsed, &ctx, false, "f").is_err());

        let mut subject = "x".to_string();
        for _ in 0..40 {
            subject = format!("(match {subject}: 0 => 0 1 => 0 _ => 0)");
        }
        let parsed =
            parse_at(&format!("match {subject}: _ => 0"), MAX_FORMULA_DEPTH).expect("parses");
        let mut size = LoweringSize::new(&ctx);
        assert_eq!(size.scalar(&parsed), 1);
        // `S(k + 1) = 2 S(k) + 9` from `S(0) = 1`: two arms each copy it.
        assert_eq!(size.dropped, 10 * (1 << 40) - 9);
        assert_eq!(
            check_lowered_size(&parsed, &ctx, false, "f")
                .expect_err("refused")
                .to_string(),
            format!(
                "formula lower error: the formula of `f` expands to more than {max} expression nodes; split it into named rules"
            )
        );
    }

    /// A `match` is as deep as the `elif` chain that does the same thing.
    #[test]
    fn a_match_is_as_deep_as_the_elif_chain_it_lowers_to() {
        for arms in 1..=12 {
            let mut matched = String::from("match x:");
            let mut chained = String::new();
            for arm in 1..=arms {
                matched.push_str(&format!(" {arm} => a"));
                chained.push_str(&format!(
                    "{} x == {arm}: a",
                    if arm == 1 { "if" } else { " elif" }
                ));
            }
            matched.push_str(" _ => b");
            chained.push_str(" else: b");
            assert_eq!(needed(&matched), arms + 2, "`{matched}`");
            assert_eq!(needed(&chained), arms + 2, "`{chained}`");
        }
    }

    /// The error says how deep a formula may nest and what to do, and points
    /// at the token that passes the bound.
    #[test]
    fn the_depth_error_points_at_the_level_past_the_bound() {
        let located = |formula: &str| match parse_at(formula, 3) {
            Err(FormulaError::Parse { line, col, message }) => (line, col, message),
            other => panic!("`{formula}` at a bound of 3: {other:?}"),
        };
        let message =
            "formula nests more than 3 levels deep; split it into named rules".to_string();
        // The fourth group.
        assert_eq!(located("((((1))))"), (1, 4, message.clone()));
        // The third `+`, which would build the fourth level.
        assert_eq!(located("1 + 1 + 1 + 1"), (1, 11, message.clone()));
        // The second `elif`.
        assert_eq!(
            located("if a: 1 elif b: 2 elif c: 3 else: 4"),
            (1, 19, message.clone())
        );
        // The second arm.
        assert_eq!(located("match x: 1 => a 2 => b _ => c"), (1, 17, message));

        // The default bound: a whole formula is one level, so the group that
        // passes it is the `MAX_FORMULA_DEPTH`th.
        let error = Parser::new(
            Lexer::new(&format!("{}1", "(".repeat(MAX_FORMULA_DEPTH)))
                .tokenise()
                .expect("parentheses tokenise"),
        )
        .parse_expr()
        .expect_err("past the bound");
        assert_eq!(
            error.to_string(),
            format!(
                "formula parse error at line 1, col {}: formula nests more than {MAX_FORMULA_DEPTH} levels deep; split it into named rules",
                MAX_FORMULA_DEPTH + 1
            )
        );
    }

    /// Every way into the parser is bounded: a declaration's dated value, an
    /// amendment's (parsed and discarded), the non-exhaustive `match` count,
    /// and a relation predicate.
    #[test]
    fn every_entry_point_refuses_a_formula_past_the_bound() {
        on_stack(|| {
            let is_too_deep = |error: FormulaError| {
                error.to_string().contains(&format!(
                    "formula nests more than {MAX_FORMULA_DEPTH} levels deep"
                ))
            };
            for levels in [MAX_FORMULA_DEPTH, 50_000] {
                // `levels` prefix operators nest `levels + 1` deep; one fewer
                // than the bound is the deepest formula accepted.
                for (past, within) in [
                    ("-".repeat(levels), "-".repeat(MAX_FORMULA_DEPTH - 1)),
                    ("not ".repeat(levels), "not ".repeat(MAX_FORMULA_DEPTH - 1)),
                    ("(".repeat(levels), "(".repeat(MAX_FORMULA_DEPTH - 1)),
                    ("max(".repeat(levels), "max(".repeat(MAX_FORMULA_DEPTH - 1)),
                ] {
                    let close = |open: &str| {
                        let closer = if open.contains('(') { ")" } else { "" };
                        let count = open.matches(['-', '(']).count() + open.matches("not").count();
                        format!("{open}x{}", closer.repeat(count))
                    };
                    let (past, within) = (close(&past), close(&within));

                    let declaration =
                        |formula: &str| format!("deep:\n  from 2026-01-01: {formula}");
                    assert!(is_too_deep(
                        parse_source(&declaration(&past)).expect_err("a declaration's value")
                    ));
                    parse_source(&declaration(&within)).expect("a declaration at the bound");

                    let amendment =
                        |formula: &str| format!("amend deep:\n  from 2026-01-01: {formula}");
                    assert!(is_too_deep(
                        parse_source(&amendment(&past)).expect_err("an amendment's value")
                    ));
                    parse_source(&amendment(&within)).expect("an amendment at the bound");

                    assert!(is_too_deep(
                        non_exhaustive_match_count(&past).expect_err("the match count")
                    ));
                    non_exhaustive_match_count(&within).expect("the match count at the bound");

                    assert!(is_too_deep(
                        lower_judgment_formula("p", &past, HashSet::new(), HashSet::new())
                            .expect_err("a relation predicate")
                    ));
                }
            }
            lower_judgment_formula(
                "p",
                &format!("{}x", "not ".repeat(MAX_FORMULA_DEPTH - 1)),
                HashSet::new(),
                HashSet::new(),
            )
            .expect("a relation predicate at the bound");
        });
    }

    fn inputs_read(shape: &Shape, names: &mut std::collections::BTreeSet<&'static str>) {
        match shape {
            Shape::Int(_) | Shape::Bool(_) => {}
            Shape::Var(name) => {
                names.insert(name);
            }
            Shape::Unary(_, operand) | Shape::Group(operand) | Shape::Field(operand, _) => {
                inputs_read(operand, names)
            }
            Shape::Binary(_, left, right) | Shape::Index(left, right) => {
                inputs_read(left, names);
                inputs_read(right, names);
            }
            Shape::Call(_, args) => args.iter().for_each(|arg| inputs_read(arg, names)),
            Shape::Cond(branches, else_expr) => {
                for (condition, then_expr) in branches {
                    inputs_read(condition, names);
                    inputs_read(then_expr, names);
                }
                inputs_read(else_expr, names);
            }
            Shape::Match(subject, arms, default) => {
                inputs_read(subject, names);
                arms.iter()
                    .for_each(|(_, result)| inputs_read(result, names));
                default
                    .iter()
                    .for_each(|default| inputs_read(default, names));
            }
        }
    }

    /// `shape` nested `levels` deeper, one way or several. A generated
    /// formula is small, because a `match` lowers its subject once per arm;
    /// this reaches the bound without growing the tree.
    fn deepen(rng: &mut Rng, shape: Shape, levels: usize) -> Shape {
        let literal = |value: usize| Shape::Int(i64::try_from(value).expect("small literal"));
        match rng.below(6) {
            0 => (0..levels).fold(shape, |sum, term| {
                binary(BinOpKind::Add, sum, literal(term))
            }),
            1 if levels > 0 => Shape::Cond(
                (0..levels)
                    .map(|branch| {
                        (
                            binary(BinOpKind::Eq, Shape::Var("a"), literal(branch)),
                            literal(branch),
                        )
                    })
                    .collect(),
                Box::new(shape),
            ),
            2 => Shape::Match(
                Box::new(Shape::Var("a")),
                (0..levels)
                    .map(|arm| (i64::try_from(arm).expect("small pattern"), literal(arm)))
                    .collect(),
                Some(Box::new(at_least(shape, 1))),
            ),
            3 => (0..levels).fold(shape, |operand, _| {
                Shape::Unary(UnaryOpKind::Neg, Box::new(at_least(operand, 6)))
            }),
            4 => (0..levels).fold(shape, |arg, _| Shape::Call("max", vec![arg, Shape::Int(0)])),
            _ => (0..levels).fold(shape, |inner, _| match rng.below(4) {
                0 => Shape::Unary(UnaryOpKind::Neg, Box::new(at_least(inner, 6))),
                1 => Shape::Call("min", vec![Shape::Var("b"), inner]),
                2 => Shape::Group(Box::new(inner)),
                _ => binary(BinOpKind::Mul, Shape::Var("c"), inner),
            }),
        }
    }

    /// Compile and load agree (#236). A generated formula either compiles or
    /// is refused as too deep, and it is refused exactly when its shape needs
    /// more than the bound. One that compiles serializes to an artifact that
    /// loads back unchanged, and the loaded artifact gives the same response
    /// or the same error, read from JSON as a host sends it.
    #[test]
    fn a_formula_compiles_to_an_artifact_that_reloads_and_answers_the_same() {
        use crate::api::{ExecutionRequest, execute_request};
        use crate::compile::CompiledProgramArtifact;

        on_stack(|| {
            let (mut compiled, mut refused, mut deepest_json) = (0, 0, 0);
            for seed in 0..600_u64 {
                let mut rng = Rng(seed.wrapping_add(0x9e37_79b9_7f4a_7c15));
                let size = 1 + usize::try_from(seed % 16).expect("small size");
                let levels = rng.below(150);
                let shape = generate_scalar(&mut rng, size);
                let shape = deepen(&mut rng, shape, levels);
                let formula = render(&shape);
                let need = bound_needed(&shape);
                let context = format!("seed {seed}: needs {need}: `{formula}`");
                let source = format!(
                    "format: rulespec/v1\nrules:\n  - name: out\n    kind: derived\n    entity: Household\n    dtype: Decimal\n    period: Month\n    versions:\n      - effective_from: 2026-01-01\n        formula: \"{formula}\"\n"
                );
                let artifact = match CompiledProgramArtifact::from_rulespec_str(&source) {
                    Ok(artifact) => artifact,
                    Err(error) => {
                        assert!(
                            need > MAX_FORMULA_DEPTH
                                && error.to_string().contains("levels deep; split it"),
                            "{context}: {error}"
                        );
                        refused += 1;
                        continue;
                    }
                };
                assert!(
                    need <= MAX_FORMULA_DEPTH,
                    "{context}: compiled past the bound"
                );
                compiled += 1;

                let json = serde_json::to_string(&artifact).expect("artifact serializes");
                deepest_json = deepest_json.max(object_depth_with_arrays(
                    &serde_json::from_value(serde_json::to_value(&artifact).expect("value"))
                        .expect("value"),
                ));
                let reloaded = CompiledProgramArtifact::from_json_str(&json)
                    .unwrap_or_else(|error| panic!("{context}: does not reload: {error}"));
                assert_eq!(
                    serde_json::to_string(&reloaded).expect("artifact serializes"),
                    json,
                    "{context}: reloading changed the artifact"
                );

                let mut names = std::collections::BTreeSet::new();
                inputs_read(&shape, &mut names);
                let inputs: Vec<serde_json::Value> = names
                    .iter()
                    .map(|name| {
                        let value = match *name {
                            "p" => serde_json::json!({"kind": "bool", "value": true}),
                            "q" => serde_json::json!({"kind": "bool", "value": false}),
                            name => serde_json::json!({
                                "kind": "integer",
                                "value": 1 + i64::from(name.as_bytes()[0] - b'a'),
                            }),
                        };
                        serde_json::json!({
                            "name": name, "entity": "Household", "entity_id": "h1",
                            "interval": {"start": "2026-01-01", "end": "2026-01-31"},
                            "value": value,
                        })
                    })
                    .collect();
                let answer = |artifact: &CompiledProgramArtifact, mode: &str| {
                    let request = serde_json::json!({
                        "mode": mode,
                        "program": artifact.program,
                        "dataset": {"inputs": inputs, "relations": []},
                        "queries": [{
                            "entity_id": "h1",
                            "period": {"period_kind": "month", "start": "2026-01-01", "end": "2026-01-31"},
                            "outputs": ["out"],
                        }],
                    })
                    .to_string();
                    ExecutionRequest::from_json_str(&request)
                        .map_err(|error| format!("request: {error}"))
                        .and_then(|request| {
                            execute_request(request).map_err(|error| error.to_string())
                        })
                        .map(|response| {
                            serde_json::to_string(&response).expect("response serializes")
                        })
                };
                for mode in ["explain", "fast"] {
                    let first = answer(&artifact, mode);
                    assert!(
                        !first
                            .as_ref()
                            .is_err_and(|error| error.starts_with("request:")),
                        "{context}: {first:?}"
                    );
                    assert_eq!(
                        answer(&reloaded, mode),
                        first,
                        "{context}: the reloaded artifact answers differently in {mode}"
                    );
                }
            }
            assert!(
                compiled > 300 && refused > 20,
                "the generator compiled {compiled} formulas and had {refused} refused"
            );
            // Past serde_json's own 128 levels, which is what #236 was about.
            assert!(
                deepest_json > 200,
                "the deepest artifact nested only {deepest_json} levels of JSON"
            );
        });
    }

    /// How deep a JSON value nests, in arrays and objects.
    fn object_depth_with_arrays(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::Object(fields) => {
                1 + fields
                    .values()
                    .map(object_depth_with_arrays)
                    .max()
                    .unwrap_or(0)
            }
            serde_json::Value::Array(items) => {
                1 + items
                    .iter()
                    .map(object_depth_with_arrays)
                    .max()
                    .unwrap_or(0)
            }
            _ => 0,
        }
    }

    /// Openers of every kind, mixed and nested far past the bound, are
    /// refused; the same mix nests exactly one level per opener.
    #[test]
    fn mixed_nesting_is_one_level_per_opener() {
        on_stack(|| {
            const OPENERS: [(&str, &str); 6] = [
                ("(", ")"),
                ("-", ""),
                ("not ", ""),
                ("max(", ")"),
                ("t[", "]"),
                ("if c: ", " else: 0"),
            ];
            for seed in 0..200_u64 {
                let mut rng = Rng(seed.wrapping_add(0x9e37_79b9_7f4a_7c15));
                let nest = |rng: &mut Rng, levels: usize| {
                    let mut open = String::new();
                    let mut close = String::new();
                    let mut after_prefix = false;
                    for _ in 0..levels {
                        // A prefix operator's operand cannot be a bare `if`.
                        let (opener, closer) = loop {
                            let choice = rng.pick(&OPENERS);
                            if !(after_prefix && choice.0.starts_with("if")) {
                                break choice;
                            }
                        };
                        after_prefix = closer.is_empty();
                        open.push_str(opener);
                        close.insert_str(0, closer);
                    }
                    format!("{open}1{close}")
                };
                let levels = rng.below(MAX_FORMULA_DEPTH);
                let formula = nest(&mut rng, levels);
                assert_eq!(needed(&formula), levels + 1, "seed {seed}: `{formula}`");

                let far = MAX_FORMULA_DEPTH + rng.below(20_000);
                let formula = nest(&mut rng, far);
                assert!(
                    too_deep(&parse_at(&formula, MAX_FORMULA_DEPTH), MAX_FORMULA_DEPTH),
                    "seed {seed}: {far} mixed levels were not refused"
                );
            }
        });
    }
}
