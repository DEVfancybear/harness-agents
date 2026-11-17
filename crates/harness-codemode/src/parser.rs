//! A recursive-descent parser for the JavaScript codemode scripts are written
//! in: statements, functions and arrows (async too), destructuring, spread,
//! optional chaining, template literals and regular expressions, with
//! automatic semicolon insertion. Classes, generators, getters and setters,
//! modules and `with` are refused with a message that says so.

use std::rc::Rc;

use crate::SyntaxError;
use crate::ast::{
    AssignOp, BinaryOp, Declarator, Element, Expr, ForHead, ForInit, Function, FunctionBody,
    LogicalOp, MemberKey, Name, Param, Pattern, PatternItem, PatternProp, PropKey, Property, Stmt,
    StmtKind, SwitchCase, UnaryOp, VarKind,
};
use crate::lexer::{Tok, Token, tokenize};

/// Deeper nesting is refused before it could exhaust the stack.
const MAX_DEPTH: usize = 300;

/// Words that cannot name a binding.
const RESERVED: [&str; 37] = [
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "let",
];

/// Parse a script: the body of an async function.
pub fn parse_script(source: &str) -> Result<Vec<Stmt>, SyntaxError> {
    let tokens = tokenize(source)?;
    let mut parser = Parser::new(tokens);
    let mut body = Vec::new();
    while !parser.at_eof() {
        body.push(parser.statement()?);
    }
    Ok(body)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    depth: usize,
    /// Inside a `for (...)` head, where `in` ends the initializer.
    no_in: bool,
}

fn binary_precedence(punct: &str) -> Option<(u8, BinaryOp)> {
    Some(match punct {
        "|" => (4, BinaryOp::BitOr),
        "^" => (5, BinaryOp::BitXor),
        "&" => (6, BinaryOp::BitAnd),
        "==" => (7, BinaryOp::Eq),
        "!=" => (7, BinaryOp::NotEq),
        "===" => (7, BinaryOp::StrictEq),
        "!==" => (7, BinaryOp::StrictNotEq),
        "<" => (8, BinaryOp::Lt),
        ">" => (8, BinaryOp::Gt),
        "<=" => (8, BinaryOp::LtEq),
        ">=" => (8, BinaryOp::GtEq),
        "<<" => (9, BinaryOp::Shl),
        ">>" => (9, BinaryOp::Shr),
        ">>>" => (9, BinaryOp::UShr),
        "+" => (10, BinaryOp::Add),
        "-" => (10, BinaryOp::Sub),
        "*" => (11, BinaryOp::Mul),
        "/" => (11, BinaryOp::Div),
        "%" => (11, BinaryOp::Rem),
        "**" => (12, BinaryOp::Pow),
        _ => return None,
    })
}

fn assign_op(punct: &str) -> Option<AssignOp> {
    Some(match punct {
        "=" => AssignOp::Assign,
        "+=" => AssignOp::Binary(BinaryOp::Add),
        "-=" => AssignOp::Binary(BinaryOp::Sub),
        "*=" => AssignOp::Binary(BinaryOp::Mul),
        "/=" => AssignOp::Binary(BinaryOp::Div),
        "%=" => AssignOp::Binary(BinaryOp::Rem),
        "**=" => AssignOp::Binary(BinaryOp::Pow),
        "<<=" => AssignOp::Binary(BinaryOp::Shl),
        ">>=" => AssignOp::Binary(BinaryOp::Shr),
        ">>>=" => AssignOp::Binary(BinaryOp::UShr),
        "&=" => AssignOp::Binary(BinaryOp::BitAnd),
        "|=" => AssignOp::Binary(BinaryOp::BitOr),
        "^=" => AssignOp::Binary(BinaryOp::BitXor),
        "&&=" => AssignOp::Logical(LogicalOp::And),
        "||=" => AssignOp::Logical(LogicalOp::Or),
        "??=" => AssignOp::Logical(LogicalOp::Nullish),
        _ => return None,
    })
}

fn describe(tok: &Tok) -> String {
    match tok {
        Tok::Num(n) => format!("number {}", crate::value::number_to_string(*n)),
        Tok::Str(_) => "string".to_owned(),
        Tok::Template(..) => "template literal".to_owned(),
        Tok::Regex(..) => "regular expression".to_owned(),
        Tok::Ident(name) => format!("'{name}'"),
        Tok::Punct(punct) => format!("'{punct}'"),
        Tok::Eof => "end of input".to_owned(),
    }
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            pos: 0,
            depth: 0,
            no_in: false,
        }
    }

    fn token(&self) -> &Token {
        &self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    fn peek_tok(&self, ahead: usize) -> &Tok {
        &self.tokens[(self.pos + ahead).min(self.tokens.len() - 1)].tok
    }

    fn line(&self) -> u32 {
        self.token().line
    }

    fn at_eof(&self) -> bool {
        matches!(self.token().tok, Tok::Eof)
    }

    fn advance(&mut self) -> Token {
        let token = self.token().clone();
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        token
    }

    fn is(&self, punct: &str) -> bool {
        matches!(&self.token().tok, Tok::Punct(p) if *p == punct)
    }

    fn is_word(&self, word: &str) -> bool {
        matches!(&self.token().tok, Tok::Ident(name) if &**name == word)
    }

    fn eat(&mut self, punct: &str) -> bool {
        if self.is(punct) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn eat_word(&mut self, word: &str) -> bool {
        if self.is_word(word) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn error_here(&self, message: impl Into<String>) -> SyntaxError {
        let token = self.token();
        SyntaxError {
            message: message.into(),
            line: token.line,
            col: token.col,
        }
    }

    fn unexpected(&self) -> SyntaxError {
        self.error_here(format!("Unexpected {}", describe(&self.token().tok)))
    }

    fn expect(&mut self, punct: &str) -> Result<(), SyntaxError> {
        if self.eat(punct) {
            Ok(())
        } else {
            Err(self.error_here(format!(
                "Unexpected {}, expected '{punct}'",
                describe(&self.token().tok)
            )))
        }
    }

    /// Automatic semicolon insertion: a `;`, or a line break, `}` or the end
    /// of input in its place.
    fn semicolon(&mut self) -> Result<(), SyntaxError> {
        if self.eat(";") || self.is("}") || self.at_eof() || self.token().newline_before {
            Ok(())
        } else {
            Err(self.unexpected())
        }
    }

    fn enter(&mut self) -> Result<(), SyntaxError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.error_here("The script is nested too deeply"));
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    /// A name for a binding.
    fn binding_name(&mut self) -> Result<Name, SyntaxError> {
        match &self.token().tok {
            Tok::Ident(name) if !RESERVED.contains(&&**name) => {
                let name = name.clone();
                self.advance();
                Ok(name)
            }
            _ => Err(self.unexpected()),
        }
    }

    /// A property name after `.`: any identifier, keywords included.
    fn property_name(&mut self) -> Result<Name, SyntaxError> {
        match &self.token().tok {
            Tok::Ident(name) => {
                let name = name.clone();
                self.advance();
                Ok(name)
            }
            Tok::Punct("#") => {
                Err(self.error_here("Private names (#name) are not supported in codemode scripts"))
            }
            _ => Err(self.unexpected()),
        }
    }

    // ---- statements -------------------------------------------------------

    fn statement(&mut self) -> Result<Stmt, SyntaxError> {
        self.enter()?;
        let result = self.statement_inner();
        self.leave();
        result
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one arm per statement keyword, in the order the grammar lists them"
    )]
    fn statement_inner(&mut self) -> Result<Stmt, SyntaxError> {
        let line = self.line();
        let kind = match self.token().tok.clone() {
            Tok::Punct("{") => StmtKind::Block(self.block()?),
            Tok::Punct(";") => {
                self.advance();
                StmtKind::Empty
            }
            Tok::Ident(word) => match &*word {
                // `let` as a name is legacy; here it always declares.
                "var" | "const" | "let" => self.var_statement()?,
                "function" => {
                    self.advance();
                    let function = self.function_rest(false, line, true)?;
                    let name = function.name.clone().ok_or_else(|| self.unexpected())?;
                    StmtKind::Function(name, function)
                }
                "async"
                    if matches!(self.peek_tok(1), Tok::Ident(w) if &**w == "function")
                        && !self.tokens[self.pos + 1].newline_before =>
                {
                    self.advance();
                    self.advance();
                    let function = self.function_rest(true, line, true)?;
                    let name = function.name.clone().ok_or_else(|| self.unexpected())?;
                    StmtKind::Function(name, function)
                }
                "class" => {
                    return Err(self.error_here(
                        "class is not supported in codemode scripts; use functions and plain objects",
                    ));
                }
                "import" | "export" => {
                    return Err(self.error_here(
                        "Modules are not available in codemode scripts: there is no import or export; call tools through `tools`",
                    ));
                }
                "with" => return Err(self.error_here("with is not supported")),
                "if" => {
                    self.advance();
                    self.expect("(")?;
                    let test = self.expression()?;
                    self.expect(")")?;
                    let then = self.statement()?;
                    let otherwise = if self.eat_word("else") {
                        Some(Box::new(self.statement()?))
                    } else {
                        None
                    };
                    StmtKind::If(test, Box::new(then), otherwise)
                }
                "for" => self.for_statement()?,
                "while" => {
                    self.advance();
                    self.expect("(")?;
                    let test = self.expression()?;
                    self.expect(")")?;
                    StmtKind::While(test, Box::new(self.statement()?))
                }
                "do" => {
                    self.advance();
                    let body = self.statement()?;
                    if !self.eat_word("while") {
                        return Err(self.unexpected());
                    }
                    self.expect("(")?;
                    let test = self.expression()?;
                    self.expect(")")?;
                    self.eat(";");
                    StmtKind::DoWhile(Box::new(body), test)
                }
                "return" => {
                    self.advance();
                    let value = if self.is(";")
                        || self.is("}")
                        || self.at_eof()
                        || self.token().newline_before
                    {
                        None
                    } else {
                        Some(self.expression()?)
                    };
                    self.semicolon()?;
                    StmtKind::Return(value)
                }
                "break" | "continue" => {
                    self.advance();
                    let label = match &self.token().tok {
                        Tok::Ident(name)
                            if !self.token().newline_before && !RESERVED.contains(&&**name) =>
                        {
                            let name = name.clone();
                            self.advance();
                            Some(name)
                        }
                        _ => None,
                    };
                    self.semicolon()?;
                    if &*word == "break" {
                        StmtKind::Break(label)
                    } else {
                        StmtKind::Continue(label)
                    }
                }
                "throw" => {
                    self.advance();
                    if self.token().newline_before {
                        return Err(self.error_here("Illegal newline after throw"));
                    }
                    let value = self.expression()?;
                    self.semicolon()?;
                    StmtKind::Throw(value)
                }
                "try" => self.try_statement()?,
                "switch" => self.switch_statement()?,
                "debugger" => {
                    self.advance();
                    self.semicolon()?;
                    StmtKind::Empty
                }
                _ if matches!(self.peek_tok(1), Tok::Punct(":")) && !RESERVED.contains(&&*word) => {
                    self.advance();
                    self.advance();
                    StmtKind::Labeled(word, Box::new(self.statement()?))
                }
                _ => self.expression_statement()?,
            },
            _ => self.expression_statement()?,
        };
        Ok(Stmt { line, kind })
    }

    fn expression_statement(&mut self) -> Result<StmtKind, SyntaxError> {
        let expr = self.expression()?;
        self.semicolon()?;
        Ok(StmtKind::Expr(expr))
    }

    fn block(&mut self) -> Result<Vec<Stmt>, SyntaxError> {
        self.expect("{")?;
        let mut body = Vec::new();
        while !self.is("}") {
            if self.at_eof() {
                return Err(self.error_here("Unexpected end of input, expected '}'"));
            }
            body.push(self.statement()?);
        }
        self.advance();
        Ok(body)
    }

    fn var_kind(&mut self) -> Option<VarKind> {
        let kind = match &self.token().tok {
            Tok::Ident(word) if &**word == "var" => VarKind::Var,
            Tok::Ident(word) if &**word == "let" => VarKind::Let,
            Tok::Ident(word) if &**word == "const" => VarKind::Const,
            _ => return None,
        };
        self.advance();
        Some(kind)
    }

    fn var_statement(&mut self) -> Result<StmtKind, SyntaxError> {
        let kind = self.var_kind().ok_or_else(|| self.unexpected())?;
        let declarators = self.declarators(kind, None)?;
        self.semicolon()?;
        Ok(StmtKind::Var(kind, declarators))
    }

    /// The declarators of a `var`/`let`/`const`, the first one's target
    /// already read when `first` is given.
    fn declarators(
        &mut self,
        kind: VarKind,
        first: Option<Pattern>,
    ) -> Result<Vec<Declarator>, SyntaxError> {
        let mut declarators = Vec::new();
        let mut first = first;
        loop {
            let target = match first.take() {
                Some(target) => target,
                None => self.binding_target()?,
            };
            let init = if self.eat("=") {
                Some(self.assignment()?)
            } else {
                if kind == VarKind::Const && !self.no_in {
                    return Err(self.error_here("Missing initializer in const declaration"));
                }
                None
            };
            declarators.push(Declarator { target, init });
            if !self.eat(",") {
                return Ok(declarators);
            }
        }
    }

    fn for_statement(&mut self) -> Result<StmtKind, SyntaxError> {
        self.advance();
        let awaiting = self.eat_word("await");
        self.expect("(")?;
        let saved_no_in = self.no_in;
        self.no_in = true;
        let init = if self.is(";") {
            None
        } else if let Some(kind) = self.var_kind() {
            let target = self.binding_target()?;
            if self.is_word("of") || self.is_word("in") {
                self.no_in = saved_no_in;
                return self.for_each(ForHead::Var(kind, target), awaiting);
            }
            Some(ForInit::Var(kind, self.declarators(kind, Some(target))?))
        } else {
            let expr = self.expression()?;
            if self.is_word("of") || self.is_word("in") {
                self.no_in = saved_no_in;
                let target = self.to_pattern(expr)?;
                return self.for_each(ForHead::Target(target), awaiting);
            }
            Some(ForInit::Expr(expr))
        };
        self.no_in = saved_no_in;
        if awaiting {
            return Err(self.error_here("for await needs of"));
        }
        self.expect(";")?;
        let test = if self.is(";") {
            None
        } else {
            Some(self.expression()?)
        };
        self.expect(";")?;
        let update = if self.is(")") {
            None
        } else {
            Some(self.expression()?)
        };
        self.expect(")")?;
        let body = Box::new(self.statement()?);
        Ok(StmtKind::For {
            init,
            test,
            update,
            body,
        })
    }

    fn for_each(&mut self, head: ForHead, awaiting: bool) -> Result<StmtKind, SyntaxError> {
        let keys = self.is_word("in");
        self.advance();
        let iterable = if keys {
            self.expression()?
        } else {
            self.assignment()?
        };
        self.expect(")")?;
        let body = self.statement()?;
        // `for await (x of list)`: each item is awaited, which an item that
        // is not a promise passes through.
        let iterable = if awaiting {
            Expr::Call {
                callee: Box::new(Expr::Ident("__forAwait".into())),
                args: vec![Element::Expr(iterable)],
                optional: false,
            }
        } else {
            iterable
        };
        Ok(StmtKind::ForEach {
            head,
            iterable,
            body: Box::new(body),
            keys,
        })
    }

    fn try_statement(&mut self) -> Result<StmtKind, SyntaxError> {
        self.advance();
        let block = self.block()?;
        let mut param = None;
        let mut handler = None;
        if self.eat_word("catch") {
            if self.eat("(") {
                param = Some(self.binding_target()?);
                self.expect(")")?;
            }
            handler = Some(self.block()?);
        }
        let finalizer = if self.eat_word("finally") {
            Some(self.block()?)
        } else {
            None
        };
        if handler.is_none() && finalizer.is_none() {
            return Err(self.error_here("Missing catch or finally after try"));
        }
        Ok(StmtKind::Try {
            block,
            param,
            handler,
            finalizer,
        })
    }

    fn switch_statement(&mut self) -> Result<StmtKind, SyntaxError> {
        self.advance();
        self.expect("(")?;
        let discriminant = self.expression()?;
        self.expect(")")?;
        self.expect("{")?;
        let mut cases = Vec::new();
        while !self.eat("}") {
            let test = if self.eat_word("case") {
                Some(self.expression()?)
            } else if self.eat_word("default") {
                None
            } else {
                return Err(self.unexpected());
            };
            self.expect(":")?;
            let mut body = Vec::new();
            while !self.is_word("case") && !self.is_word("default") && !self.is("}") {
                if self.at_eof() {
                    return Err(self.unexpected());
                }
                body.push(self.statement()?);
            }
            cases.push(SwitchCase { test, body });
        }
        Ok(StmtKind::Switch(discriminant, cases))
    }

    // ---- patterns ---------------------------------------------------------

    /// A binding target: a name, `[...]` or `{...}`.
    fn binding_target(&mut self) -> Result<Pattern, SyntaxError> {
        if self.is("[") {
            self.advance();
            let mut items = Vec::new();
            let mut rest = None;
            while !self.eat("]") {
                if self.eat(",") {
                    items.push(None);
                    continue;
                }
                if self.eat("...") {
                    rest = Some(Box::new(self.binding_target()?));
                    self.eat(",");
                    self.expect("]")?;
                    break;
                }
                let target = self.binding_target()?;
                let default = if self.eat("=") {
                    Some(self.assignment()?)
                } else {
                    None
                };
                items.push(Some(PatternItem { target, default }));
                if !self.is("]") {
                    self.expect(",")?;
                }
            }
            return Ok(Pattern::Array { items, rest });
        }
        if self.is("{") {
            self.advance();
            let mut props = Vec::new();
            let mut rest = None;
            while !self.eat("}") {
                if self.eat("...") {
                    rest = Some(self.binding_name()?);
                    self.eat(",");
                    self.expect("}")?;
                    break;
                }
                let key = self.property_key()?;
                let target = if self.eat(":") {
                    self.binding_target()?
                } else {
                    match &key {
                        PropKey::Name(name) if !RESERVED.contains(&&**name) => {
                            Pattern::Ident(name.clone())
                        }
                        _ => return Err(self.unexpected()),
                    }
                };
                let default = if self.eat("=") {
                    Some(self.assignment()?)
                } else {
                    None
                };
                props.push(PatternProp {
                    key,
                    target,
                    default,
                });
                if !self.is("}") {
                    self.expect(",")?;
                }
            }
            return Ok(Pattern::Object { props, rest });
        }
        Ok(Pattern::Ident(self.binding_name()?))
    }

    /// An expression on the left of `=` (or of `of`/`in`) as the target it
    /// stands for: `[a, b] = [b, a]`, `({x} = point)`, `obj.key = 1`.
    fn to_pattern(&self, expr: Expr) -> Result<Pattern, SyntaxError> {
        match expr {
            Expr::Ident(name) => Ok(Pattern::Ident(name)),
            expr @ Expr::Member { .. } => Ok(Pattern::Member(Box::new(expr))),
            Expr::Array(elements) => {
                let mut items = Vec::new();
                let mut rest = None;
                let count = elements.len();
                for (index, element) in elements.into_iter().enumerate() {
                    match element {
                        Element::Hole => items.push(None),
                        Element::Spread(expr) if index + 1 == count => {
                            rest = Some(Box::new(self.to_pattern(expr)?));
                        }
                        Element::Spread(_) => {
                            return Err(self.error_here("Rest element must be last element"));
                        }
                        Element::Expr(expr) => items.push(Some(self.to_item(expr)?)),
                    }
                }
                Ok(Pattern::Array { items, rest })
            }
            Expr::Object(properties) => {
                let mut props = Vec::new();
                let mut rest = None;
                for property in properties {
                    match property {
                        Property::Shorthand(name) => props.push(PatternProp {
                            key: PropKey::Name(name.clone()),
                            target: Pattern::Ident(name),
                            default: None,
                        }),
                        Property::Value(key, value) => {
                            let item = self.to_item(value)?;
                            props.push(PatternProp {
                                key,
                                target: item.target,
                                default: item.default,
                            });
                        }
                        Property::Spread(Expr::Ident(name)) => rest = Some(name),
                        Property::Spread(_) => {
                            return Err(self.error_here("Invalid rest element"));
                        }
                    }
                }
                Ok(Pattern::Object { props, rest })
            }
            _ => Err(self.error_here("Invalid left-hand side in assignment")),
        }
    }

    fn to_item(&self, expr: Expr) -> Result<PatternItem, SyntaxError> {
        match expr {
            Expr::Assign(AssignOp::Assign, target, default) => Ok(PatternItem {
                target: *target,
                default: Some(*default),
            }),
            expr => Ok(PatternItem {
                target: self.to_pattern(expr)?,
                default: None,
            }),
        }
    }

    // ---- functions --------------------------------------------------------

    /// After `function` (and `async`): an optional name, the parameters and
    /// the body.
    fn function_rest(
        &mut self,
        is_async: bool,
        line: u32,
        needs_name: bool,
    ) -> Result<Rc<Function>, SyntaxError> {
        if self.is("*") {
            return Err(self.error_here("Generators are not supported in codemode scripts"));
        }
        let name = if matches!(self.token().tok, Tok::Ident(_)) {
            Some(self.binding_name()?)
        } else if needs_name {
            return Err(self.unexpected());
        } else {
            None
        };
        let (params, rest) = self.parameters()?;
        let body = FunctionBody::Block(self.block()?);
        Ok(Rc::new(Function {
            name,
            params,
            rest,
            body,
            arrow: false,
            is_async,
            line,
        }))
    }

    fn parameters(&mut self) -> Result<(Vec<Param>, Option<Pattern>), SyntaxError> {
        self.expect("(")?;
        let mut params = Vec::new();
        let mut rest = None;
        while !self.eat(")") {
            if self.eat("...") {
                rest = Some(self.binding_target()?);
                self.eat(",");
                self.expect(")")?;
                break;
            }
            let target = self.binding_target()?;
            let default = if self.eat("=") {
                Some(self.assignment()?)
            } else {
                None
            };
            params.push(Param { target, default });
            if !self.is(")") {
                self.expect(",")?;
            }
        }
        Ok((params, rest))
    }

    /// Whether the `(` here opens an arrow function's parameters: its
    /// matching `)` is followed by `=>` on the same line.
    fn arrow_ahead(&self) -> bool {
        let mut depth = 0_usize;
        let mut index = self.pos;
        while index < self.tokens.len() {
            match &self.tokens[index].tok {
                Tok::Punct("(" | "[" | "{") => depth += 1,
                Tok::Punct(")" | "]" | "}") => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        let next = self.tokens.get(index + 1);
                        return next.is_some_and(|token| {
                            matches!(token.tok, Tok::Punct("=>")) && !token.newline_before
                        });
                    }
                }
                Tok::Eof => return false,
                _ => {}
            }
            index += 1;
        }
        false
    }

    fn arrow_body(
        &mut self,
        params: Vec<Param>,
        rest: Option<Pattern>,
        is_async: bool,
        line: u32,
    ) -> Result<Expr, SyntaxError> {
        self.expect("=>")?;
        let saved_no_in = self.no_in;
        self.no_in = false;
        let body = if self.is("{") {
            FunctionBody::Block(self.block()?)
        } else {
            FunctionBody::Expr(Box::new(self.assignment()?))
        };
        self.no_in = saved_no_in;
        Ok(Expr::Function(Rc::new(Function {
            name: None,
            params,
            rest,
            body,
            arrow: true,
            is_async,
            line,
        })))
    }

    // ---- expressions ------------------------------------------------------

    fn expression(&mut self) -> Result<Expr, SyntaxError> {
        let first = self.assignment()?;
        if !self.is(",") {
            return Ok(first);
        }
        let mut items = vec![first];
        while self.eat(",") {
            items.push(self.assignment()?);
        }
        Ok(Expr::Sequence(items))
    }

    fn assignment(&mut self) -> Result<Expr, SyntaxError> {
        self.enter()?;
        let result = self.assignment_inner();
        self.leave();
        result
    }

    fn assignment_inner(&mut self) -> Result<Expr, SyntaxError> {
        let line = self.line();
        // Arrow functions: `x => ...`, `async x => ...`, `(a, b) => ...`,
        // `async (a) => ...`.
        if let Tok::Ident(word) = self.token().tok.clone() {
            let next = self.peek_tok(1).clone();
            if matches!(next, Tok::Punct("=>")) && !RESERVED.contains(&&*word) {
                let name = self.binding_name()?;
                let params = vec![Param {
                    target: Pattern::Ident(name),
                    default: None,
                }];
                return self.arrow_body(params, None, false, line);
            }
            if &*word == "async" && !self.tokens[self.pos + 1].newline_before {
                if let Tok::Ident(param) = &next
                    && matches!(self.peek_tok(2), Tok::Punct("=>"))
                    && !RESERVED.contains(&&**param)
                {
                    self.advance();
                    let name = self.binding_name()?;
                    let params = vec![Param {
                        target: Pattern::Ident(name),
                        default: None,
                    }];
                    return self.arrow_body(params, None, true, line);
                }
                if matches!(next, Tok::Punct("(")) {
                    self.advance();
                    if self.arrow_ahead() {
                        let (params, rest) = self.parameters()?;
                        return self.arrow_body(params, rest, true, line);
                    }
                    // `async(...)`: a call of something named async.
                    self.pos -= 1;
                }
            }
        }
        if self.is("(") && self.arrow_ahead() {
            let (params, rest) = self.parameters()?;
            return self.arrow_body(params, rest, false, line);
        }
        let left = self.conditional()?;
        if let Tok::Punct(punct) = self.token().tok
            && let Some(op) = assign_op(punct)
        {
            self.advance();
            let target = match op {
                AssignOp::Assign => self.to_pattern(left)?,
                _ => match left {
                    Expr::Ident(name) => Pattern::Ident(name),
                    member @ Expr::Member { .. } => Pattern::Member(Box::new(member)),
                    _ => return Err(self.error_here("Invalid left-hand side in assignment")),
                },
            };
            let value = self.assignment()?;
            return Ok(Expr::Assign(op, Box::new(target), Box::new(value)));
        }
        Ok(left)
    }

    fn conditional(&mut self) -> Result<Expr, SyntaxError> {
        let test = self.nullish()?;
        if !self.eat("?") {
            return Ok(test);
        }
        let saved_no_in = self.no_in;
        self.no_in = false;
        let then = self.assignment()?;
        self.no_in = saved_no_in;
        self.expect(":")?;
        let otherwise = self.assignment()?;
        Ok(Expr::Conditional(
            Box::new(test),
            Box::new(then),
            Box::new(otherwise),
        ))
    }

    fn nullish(&mut self) -> Result<Expr, SyntaxError> {
        let mut left = self.logical_or()?;
        while self.eat("??") {
            let right = self.logical_or()?;
            left = Expr::Logical(LogicalOp::Nullish, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn logical_or(&mut self) -> Result<Expr, SyntaxError> {
        let mut left = self.logical_and()?;
        while self.eat("||") {
            let right = self.logical_and()?;
            left = Expr::Logical(LogicalOp::Or, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn logical_and(&mut self) -> Result<Expr, SyntaxError> {
        let mut left = self.binary(4)?;
        while self.eat("&&") {
            let right = self.binary(4)?;
            left = Expr::Logical(LogicalOp::And, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// Binary operators of at least `min` precedence, by precedence climbing.
    fn binary(&mut self, min: u8) -> Result<Expr, SyntaxError> {
        let mut left = self.unary()?;
        loop {
            let (precedence, op) = match &self.token().tok {
                Tok::Punct(punct) => match binary_precedence(punct) {
                    Some(found) => found,
                    None => break,
                },
                Tok::Ident(word) if &**word == "instanceof" => (8, BinaryOp::InstanceOf),
                Tok::Ident(word) if &**word == "in" && !self.no_in => (8, BinaryOp::In),
                _ => break,
            };
            if precedence < min {
                break;
            }
            self.advance();
            // `**` is right-associative.
            let next = if op == BinaryOp::Pow {
                precedence
            } else {
                precedence + 1
            };
            let right = self.binary(next)?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, SyntaxError> {
        self.enter()?;
        let result = self.unary_inner();
        self.leave();
        result
    }

    fn unary_inner(&mut self) -> Result<Expr, SyntaxError> {
        let op = match &self.token().tok {
            Tok::Punct("!") => Some(UnaryOp::Not),
            Tok::Punct("-") => Some(UnaryOp::Neg),
            Tok::Punct("+") => Some(UnaryOp::Plus),
            Tok::Punct("~") => Some(UnaryOp::BitNot),
            Tok::Ident(word) if &**word == "typeof" => Some(UnaryOp::Typeof),
            Tok::Ident(word) if &**word == "void" => Some(UnaryOp::Void),
            Tok::Ident(word) if &**word == "delete" => Some(UnaryOp::Delete),
            _ => None,
        };
        if let Some(op) = op {
            self.advance();
            let operand = self.unary()?;
            return Ok(Expr::Unary(op, Box::new(operand)));
        }
        if self.is_word("await") {
            self.advance();
            let operand = self.unary()?;
            return Ok(Expr::Await(Box::new(operand)));
        }
        if self.is("++") || self.is("--") {
            let increment = self.is("++");
            self.advance();
            let target = self.unary()?;
            if !matches!(target, Expr::Ident(_) | Expr::Member { .. }) {
                return Err(
                    self.error_here("Invalid left-hand side expression in prefix operation")
                );
            }
            return Ok(Expr::Update {
                increment,
                prefix: true,
                target: Box::new(target),
            });
        }
        let expr = self.call_member()?;
        if (self.is("++") || self.is("--")) && !self.token().newline_before {
            let increment = self.is("++");
            if !matches!(expr, Expr::Ident(_) | Expr::Member { .. }) {
                return Err(
                    self.error_here("Invalid left-hand side expression in postfix operation")
                );
            }
            self.advance();
            return Ok(Expr::Update {
                increment,
                prefix: false,
                target: Box::new(expr),
            });
        }
        Ok(expr)
    }

    fn arguments(&mut self) -> Result<Vec<Element>, SyntaxError> {
        self.expect("(")?;
        let saved_no_in = self.no_in;
        self.no_in = false;
        let mut args = Vec::new();
        while !self.eat(")") {
            if self.eat("...") {
                args.push(Element::Spread(self.assignment()?));
            } else {
                args.push(Element::Expr(self.assignment()?));
            }
            if !self.is(")") {
                self.expect(",")?;
            }
        }
        self.no_in = saved_no_in;
        Ok(args)
    }

    fn call_member(&mut self) -> Result<Expr, SyntaxError> {
        let mut expr = if self.is_word("new") {
            self.new_expression()?
        } else {
            self.primary()?
        };
        let mut chained = false;
        loop {
            if self.eat(".") {
                let key = MemberKey::Name(self.property_name()?);
                expr = Expr::Member {
                    object: Box::new(expr),
                    key,
                    optional: false,
                };
            } else if self.eat("?.") {
                chained = true;
                if self.is("(") {
                    let args = self.arguments()?;
                    expr = Expr::Call {
                        callee: Box::new(expr),
                        args,
                        optional: true,
                    };
                } else if self.eat("[") {
                    let key = self.expression()?;
                    self.expect("]")?;
                    expr = Expr::Member {
                        object: Box::new(expr),
                        key: MemberKey::Computed(Box::new(key)),
                        optional: true,
                    };
                } else {
                    let key = MemberKey::Name(self.property_name()?);
                    expr = Expr::Member {
                        object: Box::new(expr),
                        key,
                        optional: true,
                    };
                }
            } else if self.is("[") {
                self.advance();
                let saved_no_in = self.no_in;
                self.no_in = false;
                let key = self.expression()?;
                self.no_in = saved_no_in;
                self.expect("]")?;
                expr = Expr::Member {
                    object: Box::new(expr),
                    key: MemberKey::Computed(Box::new(key)),
                    optional: false,
                };
            } else if self.is("(") {
                let args = self.arguments()?;
                expr = Expr::Call {
                    callee: Box::new(expr),
                    args,
                    optional: false,
                };
            } else if matches!(self.token().tok, Tok::Template(..)) {
                return Err(
                    self.error_here("Tagged templates are not supported in codemode scripts")
                );
            } else {
                break;
            }
        }
        Ok(if chained {
            Expr::Chain(Box::new(expr))
        } else {
            expr
        })
    }

    /// `new Callee(args)`: the callee is a member expression without calls.
    fn new_expression(&mut self) -> Result<Expr, SyntaxError> {
        self.advance();
        let mut callee = if self.is_word("new") {
            self.new_expression()?
        } else {
            self.primary()?
        };
        loop {
            if self.eat(".") {
                let key = MemberKey::Name(self.property_name()?);
                callee = Expr::Member {
                    object: Box::new(callee),
                    key,
                    optional: false,
                };
            } else if self.eat("[") {
                let key = self.expression()?;
                self.expect("]")?;
                callee = Expr::Member {
                    object: Box::new(callee),
                    key: MemberKey::Computed(Box::new(key)),
                    optional: false,
                };
            } else {
                break;
            }
        }
        let args = if self.is("(") {
            self.arguments()?
        } else {
            Vec::new()
        };
        Ok(Expr::New(Box::new(callee), args))
    }

    fn property_key(&mut self) -> Result<PropKey, SyntaxError> {
        match self.token().tok.clone() {
            Tok::Ident(name) => {
                self.advance();
                Ok(PropKey::Name(name))
            }
            Tok::Str(text) => {
                self.advance();
                Ok(PropKey::Name(text))
            }
            Tok::Num(number) => {
                self.advance();
                Ok(PropKey::Name(crate::value::number_to_string(number).into()))
            }
            Tok::Punct("[") => {
                self.advance();
                let key = self.assignment()?;
                self.expect("]")?;
                Ok(PropKey::Computed(key))
            }
            _ => Err(self.unexpected()),
        }
    }

    fn object_literal(&mut self) -> Result<Expr, SyntaxError> {
        self.expect("{")?;
        let saved_no_in = self.no_in;
        self.no_in = false;
        let mut properties = Vec::new();
        while !self.eat("}") {
            if self.eat("...") {
                properties.push(Property::Spread(self.assignment()?));
            } else {
                let line = self.line();
                let is_async = self.is_word("async")
                    && !matches!(self.peek_tok(1), Tok::Punct("," | ":" | "(" | "}" | "="));
                if is_async {
                    self.advance();
                }
                if self.is("*") {
                    return Err(self.error_here("Generators are not supported in codemode scripts"));
                }
                if (self.is_word("get") || self.is_word("set"))
                    && !matches!(self.peek_tok(1), Tok::Punct("," | ":" | "(" | "}" | "="))
                {
                    return Err(self
                        .error_here("Getters and setters are not supported in codemode scripts"));
                }
                let shorthand = match &self.token().tok {
                    Tok::Ident(name) => Some(name.clone()),
                    _ => None,
                };
                let key = self.property_key()?;
                if self.eat(":") {
                    properties.push(Property::Value(key, self.assignment()?));
                } else if self.is("(") {
                    let (params, rest) = self.parameters()?;
                    let body = FunctionBody::Block(self.block()?);
                    let name = match &key {
                        PropKey::Name(name) => Some(name.clone()),
                        PropKey::Computed(_) => None,
                    };
                    properties.push(Property::Value(
                        key,
                        Expr::Function(Rc::new(Function {
                            name,
                            params,
                            rest,
                            body,
                            arrow: false,
                            is_async,
                            line,
                        })),
                    ));
                } else if let Some(name) = shorthand {
                    if self.is("=") {
                        // `{ a = 1 }` is only valid as a pattern.
                        self.advance();
                        let default = self.assignment()?;
                        properties.push(Property::Value(
                            PropKey::Name(name.clone()),
                            Expr::Assign(
                                AssignOp::Assign,
                                Box::new(Pattern::Ident(name)),
                                Box::new(default),
                            ),
                        ));
                    } else {
                        properties.push(Property::Shorthand(name));
                    }
                } else {
                    return Err(self.unexpected());
                }
            }
            if !self.is("}") {
                self.expect(",")?;
            }
        }
        self.no_in = saved_no_in;
        Ok(Expr::Object(properties))
    }

    fn array_literal(&mut self) -> Result<Expr, SyntaxError> {
        self.expect("[")?;
        let saved_no_in = self.no_in;
        self.no_in = false;
        let mut elements = Vec::new();
        while !self.eat("]") {
            if self.eat(",") {
                elements.push(Element::Hole);
                continue;
            }
            if self.eat("...") {
                elements.push(Element::Spread(self.assignment()?));
            } else {
                elements.push(Element::Expr(self.assignment()?));
            }
            if !self.is("]") {
                self.expect(",")?;
            }
        }
        self.no_in = saved_no_in;
        Ok(Expr::Array(elements))
    }

    fn primary(&mut self) -> Result<Expr, SyntaxError> {
        let line = self.line();
        match self.token().tok.clone() {
            Tok::Num(number) => {
                self.advance();
                Ok(Expr::Number(number))
            }
            Tok::Str(text) => {
                self.advance();
                Ok(Expr::Str(text))
            }
            Tok::Template(quasis, substitutions) => {
                self.advance();
                let mut exprs = Vec::new();
                for tokens in substitutions {
                    let mut inner = Parser::new(tokens);
                    inner.depth = self.depth;
                    let expr = inner.expression()?;
                    if !inner.at_eof() {
                        return Err(inner.unexpected());
                    }
                    exprs.push(expr);
                }
                Ok(Expr::Template(quasis, exprs))
            }
            Tok::Regex(pattern, flags) => {
                self.advance();
                Ok(Expr::Regex(pattern, flags))
            }
            Tok::Punct("(") => {
                self.advance();
                let saved_no_in = self.no_in;
                self.no_in = false;
                let expr = self.expression()?;
                self.no_in = saved_no_in;
                self.expect(")")?;
                Ok(expr)
            }
            Tok::Punct("[") => self.array_literal(),
            Tok::Punct("{") => self.object_literal(),
            Tok::Ident(word) => match &*word {
                "this" => {
                    self.advance();
                    Ok(Expr::This)
                }
                "true" | "false" => {
                    self.advance();
                    Ok(Expr::Bool(&*word == "true"))
                }
                "null" => {
                    self.advance();
                    Ok(Expr::Null)
                }
                "function" => {
                    self.advance();
                    Ok(Expr::Function(self.function_rest(false, line, false)?))
                }
                "async"
                    if matches!(self.peek_tok(1), Tok::Ident(w) if &**w == "function")
                        && !self.tokens[self.pos + 1].newline_before =>
                {
                    self.advance();
                    self.advance();
                    Ok(Expr::Function(self.function_rest(true, line, false)?))
                }
                "class" => Err(self.error_here(
                    "class is not supported in codemode scripts; use functions and plain objects",
                )),
                "import" => Err(self.error_here(
                    "import() is not available in codemode scripts: call tools through `tools`",
                )),
                "super" | "yield" => Err(self.unexpected()),
                _ if RESERVED.contains(&&*word) && &*word != "let" => Err(self.unexpected()),
                _ => {
                    self.advance();
                    Ok(Expr::Ident(word))
                }
            },
            _ => Err(self.unexpected()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_script;
    use crate::ast::{Expr, StmtKind};

    #[test]
    fn parses_statements_with_and_without_semicolons() {
        let body = parse_script(
            "const a = 1\nlet b = a + 2;\nif (b > 2) { b++ } else b--\nfor (const x of [1, 2]) text(x)\nreturn\nb",
        )
        .unwrap();
        assert_eq!(body.len(), 6);
        assert!(matches!(body[4].kind, StmtKind::Return(None)));
    }

    #[test]
    fn parses_arrows_destructuring_and_chains() {
        let body = parse_script(
            "const { items = [], ...rest } = await tools.list({ q: `a${1}` });\n\
             const f = async ({ a }, [b, , c] = [], ...more) => a?.b?.[c] ?? b;\n\
             [x, y] = [y, x];\n\
             const g = x => ({ x });",
        )
        .unwrap();
        assert_eq!(body.len(), 4);
        let StmtKind::Expr(Expr::Assign(..)) = &body[2].kind else {
            panic!("{:?}", body[2].kind);
        };
    }

    #[test]
    fn parses_loops_switch_try_and_labels() {
        parse_script(
            "outer: for (let i = 0, j = 10; i < j; i++, j--) { for (const k in obj) { if (k) continue outer; } }\n\
             switch (x) { case 1: case 2: y(); break; default: z() }\n\
             try { a() } catch ({ message }) { b(message) } finally { c() }\n\
             do { n-- } while (n > 0)\n\
             for await (const r of list) text(r)",
        )
        .unwrap();
    }

    #[test]
    fn says_which_line_is_wrong_and_what_is_not_supported() {
        let error = parse_script("const a = 1;\nconst b = ;").unwrap_err();
        assert_eq!(error.line, 2);
        assert!(error.message.contains("Unexpected"), "{}", error.message);
        for (source, needle) in [
            ("class A {}", "class"),
            ("import x from 'y'", "Modules"),
            ("function* g() {}", "Generators"),
            ("const o = { get x() { return 1 } }", "Getters"),
            ("const c;", "initializer"),
        ] {
            let error = parse_script(source).unwrap_err();
            assert!(
                error.message.contains(needle),
                "{source}: {}",
                error.message
            );
        }
    }

    #[test]
    fn refuses_nesting_past_the_limit() {
        // Scripts are parsed on the runner's thread, whose stack is large.
        let parsed = std::thread::Builder::new()
            .stack_size(256 * 1024 * 1024)
            .spawn(|| {
                let deep = format!("{}1{}", "(".repeat(2000), ")".repeat(2000));
                parse_script(&deep).map(|_| ())
            })
            .unwrap()
            .join()
            .unwrap();
        assert!(parsed.unwrap_err().message.contains("nested"));
    }
}
