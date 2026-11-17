//! The parser: tokens to [`Program`].
//!
//! Statements are separated by a newline or `;`. Expressions are `i32` only and
//! follow the usual precedence: `or` < `and` < comparisons < `+ -` < `* / %` <
//! prefix `- not` < primary. There are no user functions and no scopes: every
//! `let` adds a function-wide local that must be declared before it is used.

use crate::ast::{BinOp, Expr, ExprKind, Program, Stmt, StmtKind, UnOp};
use crate::error::Error;
use crate::lexer::{Kind, Token};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// Parses a token stream into a program.
pub(crate) fn parse(tokens: Vec<Token>) -> Result<Program, Error> {
    let mut parser = Parser { tokens, pos: 0 };
    parser.skip_newlines();
    let mut statements = Vec::new();
    while !parser.at_eof() {
        statements.push(parser.statement()?);
        if !parser.at_eof() && !parser.at_newline() && !parser.at_rbrace() {
            let token = parser.peek();
            return Err(Error::new(
                token.line,
                token.column,
                "expected a new line or ';' after the statement",
            ));
        }
        parser.skip_newlines();
    }
    Ok(Program { statements })
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn advance(&mut self) -> Token {
        let token = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        token
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek().kind, Kind::Eof)
    }

    fn at_newline(&self) -> bool {
        matches!(self.peek().kind, Kind::Newline)
    }

    fn at_rbrace(&self) -> bool {
        matches!(self.peek().kind, Kind::RBrace)
    }

    fn skip_newlines(&mut self) {
        while self.at_newline() {
            self.advance();
        }
    }

    fn expect(&mut self, kind: Kind, what: &str) -> Result<Token, Error> {
        if self.peek().kind == kind {
            Ok(self.advance())
        } else {
            let token = self.peek();
            Err(Error::new(
                token.line,
                token.column,
                format!("expected {what}, found {}", describe(&token.kind)),
            ))
        }
    }

    fn expect_ident(&mut self, what: &str) -> Result<(String, usize, usize), Error> {
        let token = self.peek();
        match &token.kind {
            Kind::Ident(name) => {
                let name = name.clone();
                let (line, column) = (token.line, token.column);
                self.advance();
                Ok((name, line, column))
            }
            other => Err(Error::new(
                token.line,
                token.column,
                format!("expected {what}, found {}", describe(other)),
            )),
        }
    }

    fn statement(&mut self) -> Result<Stmt, Error> {
        let token = self.peek();
        let (line, column) = (token.line, token.column);
        let kind = match &token.kind {
            Kind::Let => {
                self.advance();
                let (name, _, _) = self.expect_ident("a variable name")?;
                self.expect(Kind::Assign, "'='")?;
                StmtKind::Let(name, self.expression()?)
            }
            Kind::Log => {
                self.advance();
                let text = self.peek();
                match &text.kind {
                    Kind::Str(text) => {
                        let text = text.clone();
                        self.advance();
                        StmtKind::Log(text)
                    }
                    other => {
                        return Err(Error::new(
                            text.line,
                            text.column,
                            format!("log expects a string literal, found {}", describe(other)),
                        ))
                    }
                }
            }
            Kind::Exit => {
                self.advance();
                StmtKind::Exit(self.expression()?)
            }
            Kind::Yield => {
                self.advance();
                StmtKind::Yield
            }
            Kind::If => {
                self.advance();
                let condition = self.expression()?;
                let then = self.block()?;
                // An `else` may start on the next line, but without one the
                // newlines must stay so the caller still sees a separator.
                let mut scan = self.pos;
                while matches!(self.tokens[scan].kind, Kind::Newline) {
                    scan += 1;
                }
                let otherwise = if matches!(self.tokens[scan].kind, Kind::Else) {
                    self.pos = scan;
                    self.advance();
                    self.block()?
                } else {
                    Vec::new()
                };
                StmtKind::If(condition, then, otherwise)
            }
            Kind::While => {
                self.advance();
                let condition = self.expression()?;
                StmtKind::While(condition, self.block()?)
            }
            Kind::Ident(name) => {
                let name = name.clone();
                self.advance();
                self.expect(Kind::Assign, "'=' after the variable name")?;
                StmtKind::Assign(name, self.expression()?)
            }
            other => {
                return Err(Error::new(
                    line,
                    column,
                    format!("expected a statement, found {}", describe(other)),
                ))
            }
        };
        Ok(Stmt { kind, line, column })
    }

    fn block(&mut self) -> Result<Vec<Stmt>, Error> {
        self.expect(Kind::LBrace, "'{'")?;
        self.skip_newlines();
        let mut statements = Vec::new();
        while !self.at_rbrace() && !self.at_eof() {
            statements.push(self.statement()?);
            if !self.at_rbrace() && !self.at_eof() && !self.at_newline() {
                let token = self.peek();
                return Err(Error::new(
                    token.line,
                    token.column,
                    "expected a new line or ';' after the statement",
                ));
            }
            self.skip_newlines();
        }
        self.expect(Kind::RBrace, "'}'")?;
        Ok(statements)
    }

    fn expression(&mut self) -> Result<Expr, Error> {
        self.or()
    }

    fn or(&mut self) -> Result<Expr, Error> {
        let mut left = self.and()?;
        while matches!(self.peek().kind, Kind::Or) {
            self.advance();
            let right = self.and()?;
            let (line, column) = (left.line, left.column);
            left = binary(BinOp::Or, left, right, line, column);
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Expr, Error> {
        let mut left = self.comparison()?;
        while matches!(self.peek().kind, Kind::And) {
            self.advance();
            let right = self.comparison()?;
            let (line, column) = (left.line, left.column);
            left = binary(BinOp::And, left, right, line, column);
        }
        Ok(left)
    }

    fn comparison(&mut self) -> Result<Expr, Error> {
        let mut left = self.sum()?;
        loop {
            let op = match self.peek().kind {
                Kind::EqEq => BinOp::Eq,
                Kind::NotEq => BinOp::NotEq,
                Kind::Lt => BinOp::Lt,
                Kind::Le => BinOp::Le,
                Kind::Gt => BinOp::Gt,
                Kind::Ge => BinOp::Ge,
                _ => break,
            };
            self.advance();
            let right = self.sum()?;
            let (line, column) = (left.line, left.column);
            left = binary(op, left, right, line, column);
        }
        Ok(left)
    }

    fn sum(&mut self) -> Result<Expr, Error> {
        let mut left = self.product()?;
        loop {
            let op = match self.peek().kind {
                Kind::Plus => BinOp::Add,
                Kind::Minus => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let right = self.product()?;
            let (line, column) = (left.line, left.column);
            left = binary(op, left, right, line, column);
        }
        Ok(left)
    }

    fn product(&mut self) -> Result<Expr, Error> {
        let mut left = self.prefix()?;
        loop {
            let op = match self.peek().kind {
                Kind::Star => BinOp::Mul,
                Kind::Slash => BinOp::Div,
                Kind::Percent => BinOp::Rem,
                _ => break,
            };
            self.advance();
            let right = self.prefix()?;
            let (line, column) = (left.line, left.column);
            left = binary(op, left, right, line, column);
        }
        Ok(left)
    }

    fn prefix(&mut self) -> Result<Expr, Error> {
        let token = self.peek();
        let (line, column) = (token.line, token.column);
        let op = match token.kind {
            Kind::Minus => UnOp::Negate,
            Kind::Not => UnOp::Not,
            _ => return self.primary(),
        };
        self.advance();
        let operand = self.prefix()?;
        Ok(Expr {
            kind: ExprKind::Unary(op, Box::new(operand)),
            line,
            column,
        })
    }

    fn primary(&mut self) -> Result<Expr, Error> {
        let token = self.peek();
        let (line, column) = (token.line, token.column);
        let kind = match &token.kind {
            Kind::Int(value) => ExprKind::Int(*value),
            Kind::True => ExprKind::Int(1),
            Kind::False => ExprKind::Int(0),
            Kind::Ident(name) if name == "now_ms" => {
                self.advance();
                self.expect(Kind::LParen, "'(' after now_ms")?;
                self.expect(Kind::RParen, "')'")?;
                return Ok(Expr {
                    kind: ExprKind::NowMs,
                    line,
                    column,
                });
            }
            Kind::Ident(name) => ExprKind::Var(name.clone()),
            Kind::LParen => {
                self.advance();
                let inner = self.expression()?;
                self.expect(Kind::RParen, "')'")?;
                return Ok(inner);
            }
            Kind::Eof | Kind::Newline | Kind::RBrace => {
                return Err(Error::new(
                    line,
                    column,
                    format!("expected an expression, found {}", describe(&token.kind)),
                ))
            }
            other => {
                return Err(Error::new(
                    line,
                    column,
                    format!("expected an expression, found {}", describe(other)),
                ))
            }
        };
        self.advance();
        Ok(Expr { kind, line, column })
    }
}

fn binary(op: BinOp, left: Expr, right: Expr, line: usize, column: usize) -> Expr {
    Expr {
        kind: ExprKind::Binary(op, Box::new(left), Box::new(right)),
        line,
        column,
    }
}

/// A short description of a token for error messages.
fn describe(kind: &Kind) -> alloc::string::String {
    match kind {
        Kind::Ident(name) => format!("'{name}'"),
        Kind::Int(value) => format!("'{value}'"),
        Kind::Str(_) => alloc::string::String::from("a string"),
        Kind::Let => alloc::string::String::from("'let'"),
        Kind::If => alloc::string::String::from("'if'"),
        Kind::Else => alloc::string::String::from("'else'"),
        Kind::While => alloc::string::String::from("'while'"),
        Kind::Log => alloc::string::String::from("'log'"),
        Kind::Exit => alloc::string::String::from("'exit'"),
        Kind::Yield => alloc::string::String::from("'yield'"),
        Kind::And => alloc::string::String::from("'and'"),
        Kind::Or => alloc::string::String::from("'or'"),
        Kind::Not => alloc::string::String::from("'not'"),
        Kind::True => alloc::string::String::from("'true'"),
        Kind::False => alloc::string::String::from("'false'"),
        Kind::Plus => alloc::string::String::from("'+'"),
        Kind::Minus => alloc::string::String::from("'-'"),
        Kind::Star => alloc::string::String::from("'*'"),
        Kind::Slash => alloc::string::String::from("'/'"),
        Kind::Percent => alloc::string::String::from("'%'"),
        Kind::Assign => alloc::string::String::from("'='"),
        Kind::EqEq => alloc::string::String::from("'=='"),
        Kind::NotEq => alloc::string::String::from("'!='"),
        Kind::Lt => alloc::string::String::from("'<'"),
        Kind::Le => alloc::string::String::from("'<='"),
        Kind::Gt => alloc::string::String::from("'>'"),
        Kind::Ge => alloc::string::String::from("'>='"),
        Kind::LParen => alloc::string::String::from("'('"),
        Kind::RParen => alloc::string::String::from("')'"),
        Kind::LBrace => alloc::string::String::from("'{'"),
        Kind::RBrace => alloc::string::String::from("'}'"),
        Kind::Newline => alloc::string::String::from("the end of the line"),
        Kind::Eof => alloc::string::String::from("the end of the file"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;

    fn parse_source(source: &str) -> Result<Program, Error> {
        parse(tokenize(source)?)
    }

    #[test]
    fn parses_precedence() {
        let program = parse_source("exit 1 + 2 * 3\n").unwrap();
        let StmtKind::Exit(expr) = &program.statements[0].kind else {
            panic!("not an exit");
        };
        let ExprKind::Binary(BinOp::Add, _, right) = &expr.kind else {
            panic!("not an addition");
        };
        assert!(matches!(right.kind, ExprKind::Binary(BinOp::Mul, _, _)));
    }

    #[test]
    fn parses_blocks_and_else() {
        let program = parse_source("if 1 { log \"a\" } else { log \"b\" }\n").unwrap();
        let StmtKind::If(_, then, otherwise) = &program.statements[0].kind else {
            panic!("not an if");
        };
        assert_eq!(then.len(), 1);
        assert_eq!(otherwise.len(), 1);
    }

    #[test]
    fn requires_separators() {
        let error = parse_source("let x = 1 let y = 2\n").unwrap_err();
        assert!(error.message.contains("new line"));
        let error = parse_source("let x = 1\nlog a\n").unwrap_err();
        assert!(error.message.contains("string"));
    }

    #[test]
    fn reports_missing_tokens() {
        assert!(parse_source("let x 1\n")
            .unwrap_err()
            .message
            .contains("'='"));
        assert!(parse_source("if 1 {").unwrap_err().message.contains("'}'"));
        assert!(parse_source("exit")
            .unwrap_err()
            .message
            .contains("expression"));
    }

    #[test]
    fn unary_binds_tighter_than_multiplication() {
        let program = parse_source("exit -2 * 3\n").unwrap();
        let StmtKind::Exit(expr) = &program.statements[0].kind else {
            panic!("not an exit");
        };
        let ExprKind::Binary(BinOp::Mul, left, _) = &expr.kind else {
            panic!("not a multiplication");
        };
        assert!(matches!(left.kind, ExprKind::Unary(UnOp::Negate, _)));
    }
}
