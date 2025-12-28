//! Simplified AST for the prototype engines.
//! This is a subset of the full Boon AST, focused on the constructs
//! needed to test the toggle-all bug and basic reactive patterns.


/// Unique identifier for an expression in the AST
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExprId(pub u32);

impl ExprId {
    pub fn new(id: u32) -> Self {
        Self(id)
    }
}

/// Source location for error reporting
#[derive(Debug, Clone, Copy, Default)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

/// The main AST node type
#[derive(Debug, Clone)]
pub struct Expr {
    pub id: ExprId,
    pub kind: ExprKind,
    pub span: Span,
}

impl Expr {
    pub fn new(id: ExprId, kind: ExprKind) -> Self {
        Self {
            id,
            kind,
            span: Span::default(),
        }
    }
}

/// Expression kinds supported by the prototype
#[derive(Debug, Clone)]
pub enum ExprKind {
    /// Literal value: 42, "hello", True, False
    Literal(Literal),

    /// Variable reference: foo, bar.baz
    Variable(String),

    /// Path access: foo.bar.baz
    Path(Box<Expr>, String),

    /// Object literal: { a: 1, b: 2 }
    Object(Vec<(String, Expr)>),

    /// List literal: [1, 2, 3]
    List(Vec<Expr>),

    /// Function call: foo(a, b)
    Call(String, Vec<Expr>),

    /// Method call: expr |> method(args)
    Pipe(Box<Expr>, String, Vec<Expr>),

    /// LATEST { a, b, c }
    Latest(Vec<Expr>),

    /// initial |> HOLD state { body }
    Hold {
        initial: Box<Expr>,
        state_name: String,
        body: Box<Expr>,
    },

    /// input |> THEN { body }
    Then {
        input: Box<Expr>,
        body: Box<Expr>,
    },

    /// input |> WHEN { pattern => body, ... }
    When {
        input: Box<Expr>,
        arms: Vec<(Pattern, Expr)>,
    },

    /// input |> WHILE { pattern => body }
    While {
        input: Box<Expr>,
        pattern: Pattern,
        body: Box<Expr>,
    },

    /// LINK / LINK { alias }
    Link(Option<String>),

    /// BLOCK { bindings, output }
    Block {
        bindings: Vec<(String, Expr)>,
        output: Box<Expr>,
    },

    /// List/map(list, template)
    ListMap {
        list: Box<Expr>,
        item_name: String,
        template: Box<Expr>,
    },

    /// List/append(list, item)
    ListAppend {
        list: Box<Expr>,
        item: Box<Expr>,
    },
}

/// Pattern for WHEN/WHILE matching
#[derive(Debug, Clone)]
pub enum Pattern {
    /// Match anything, bind to name
    Bind(String),
    /// Match literal value
    Literal(Literal),
    /// Match object shape: { field: pattern, ... }
    Object(Vec<(String, Pattern)>),
    /// Wildcard: _
    Wildcard,
}

/// Literal values
#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Int(i64),
    Float(f64),
    String(String),
    Bool(bool),
    Unit,
}

/// A complete Boon program
#[derive(Debug, Clone)]
pub struct Program {
    /// Top-level bindings
    pub bindings: Vec<(String, Expr)>,
    /// Expression ID counter for generating new IDs
    next_id: u32,
}

impl Program {
    pub fn new() -> Self {
        Self {
            bindings: Vec::new(),
            next_id: 0,
        }
    }

    pub fn next_expr_id(&mut self) -> ExprId {
        let id = self.next_id;
        self.next_id += 1;
        ExprId(id)
    }

    pub fn add_binding(&mut self, name: impl Into<String>, kind: ExprKind) -> ExprId {
        let id = self.next_expr_id();
        self.bindings.push((name.into(), Expr::new(id, kind)));
        id
    }
}

impl Default for Program {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for constructing AST programmatically
pub struct AstBuilder {
    next_id: u32,
}

impl AstBuilder {
    pub fn new() -> Self {
        Self { next_id: 0 }
    }

    fn next_id(&mut self) -> ExprId {
        let id = self.next_id;
        self.next_id += 1;
        ExprId(id)
    }

    pub fn int(&mut self, value: i64) -> Expr {
        Expr::new(self.next_id(), ExprKind::Literal(Literal::Int(value)))
    }

    pub fn bool(&mut self, value: bool) -> Expr {
        Expr::new(self.next_id(), ExprKind::Literal(Literal::Bool(value)))
    }

    pub fn string(&mut self, value: impl Into<String>) -> Expr {
        Expr::new(
            self.next_id(),
            ExprKind::Literal(Literal::String(value.into())),
        )
    }

    pub fn unit(&mut self) -> Expr {
        Expr::new(self.next_id(), ExprKind::Literal(Literal::Unit))
    }

    pub fn var(&mut self, name: impl Into<String>) -> Expr {
        Expr::new(self.next_id(), ExprKind::Variable(name.into()))
    }

    pub fn path(&mut self, base: Expr, field: impl Into<String>) -> Expr {
        Expr::new(self.next_id(), ExprKind::Path(Box::new(base), field.into()))
    }

    pub fn object(&mut self, fields: Vec<(impl Into<String>, Expr)>) -> Expr {
        Expr::new(
            self.next_id(),
            ExprKind::Object(fields.into_iter().map(|(k, v)| (k.into(), v)).collect()),
        )
    }

    pub fn list(&mut self, items: Vec<Expr>) -> Expr {
        Expr::new(self.next_id(), ExprKind::List(items))
    }

    pub fn call(&mut self, name: impl Into<String>, args: Vec<Expr>) -> Expr {
        Expr::new(self.next_id(), ExprKind::Call(name.into(), args))
    }

    pub fn pipe(&mut self, input: Expr, method: impl Into<String>, args: Vec<Expr>) -> Expr {
        Expr::new(
            self.next_id(),
            ExprKind::Pipe(Box::new(input), method.into(), args),
        )
    }

    pub fn latest(&mut self, exprs: Vec<Expr>) -> Expr {
        Expr::new(self.next_id(), ExprKind::Latest(exprs))
    }

    pub fn hold(&mut self, initial: Expr, state_name: impl Into<String>, body: Expr) -> Expr {
        Expr::new(
            self.next_id(),
            ExprKind::Hold {
                initial: Box::new(initial),
                state_name: state_name.into(),
                body: Box::new(body),
            },
        )
    }

    pub fn then(&mut self, input: Expr, body: Expr) -> Expr {
        Expr::new(
            self.next_id(),
            ExprKind::Then {
                input: Box::new(input),
                body: Box::new(body),
            },
        )
    }

    pub fn when(&mut self, input: Expr, arms: Vec<(Pattern, Expr)>) -> Expr {
        Expr::new(
            self.next_id(),
            ExprKind::When {
                input: Box::new(input),
                arms,
            },
        )
    }

    pub fn while_(&mut self, input: Expr, pattern: Pattern, body: Expr) -> Expr {
        Expr::new(
            self.next_id(),
            ExprKind::While {
                input: Box::new(input),
                pattern,
                body: Box::new(body),
            },
        )
    }

    pub fn link(&mut self, alias: Option<String>) -> Expr {
        Expr::new(self.next_id(), ExprKind::Link(alias))
    }

    pub fn block(&mut self, bindings: Vec<(impl Into<String>, Expr)>, output: Expr) -> Expr {
        Expr::new(
            self.next_id(),
            ExprKind::Block {
                bindings: bindings.into_iter().map(|(k, v)| (k.into(), v)).collect(),
                output: Box::new(output),
            },
        )
    }

    pub fn list_map(&mut self, list: Expr, item_name: impl Into<String>, template: Expr) -> Expr {
        Expr::new(
            self.next_id(),
            ExprKind::ListMap {
                list: Box::new(list),
                item_name: item_name.into(),
                template: Box::new(template),
            },
        )
    }

    pub fn list_append(&mut self, list: Expr, item: Expr) -> Expr {
        Expr::new(
            self.next_id(),
            ExprKind::ListAppend {
                list: Box::new(list),
                item: Box::new(item),
            },
        )
    }

    pub fn build_program(self, bindings: Vec<(impl Into<String>, Expr)>) -> Program {
        Program {
            bindings: bindings.into_iter().map(|(k, v)| (k.into(), v)).collect(),
            next_id: self.next_id,
        }
    }
}

impl Default for AstBuilder {
    fn default() -> Self {
        Self::new()
    }
}
