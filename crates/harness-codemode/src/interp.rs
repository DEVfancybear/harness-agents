//! The evaluator: runs a parsed script as the body of an async function.
//!
//! The script has no timers and no I/O, so the only thing it can wait for is
//! a tool call. A call is made when something awaits it: `await tools.x()`
//! runs one call, `await Promise.all([...])` hands the host every call the
//! list holds at once, so the host can run them side by side. A call nothing
//! awaits never runs and is recorded as cancelled, as pi's codemode cancels
//! the calls a script left running.
//!
//! The host owns the deadline and the cancel flag; the evaluator polls both
//! every few hundred steps, so a script that spins in a loop is stopped
//! there. Running out of time, cancellation, the output limit and the memory
//! budget end the script with an error it cannot catch.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use indexmap::IndexMap;

use crate::ast::{
    AssignOp, BinaryOp, Element, Expr, ForHead, ForInit, Function as FunctionDef, FunctionBody,
    LogicalOp, MemberKey, Name, Pattern, PropKey, Property, Stmt, StmtKind, UnaryOp, VarKind,
};
use crate::builtins;
use crate::value::{
    Class, Combine, Function, Object, Promise, PromiseState, Settled, Value, loose_equals,
    strict_equals, to_key, to_number, to_string,
};
use crate::{CallRecord, CallStatus, OutputItem, ToolInfo};

/// One call the script asks the host to run: the tool, and its argument as
/// JSON text (`None` for `undefined`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostCall {
    pub name: String,
    pub args: Option<String>,
}

/// Why a run ended in a way the script cannot catch.
#[derive(Clone, Debug)]
pub enum Fatal {
    Timeout(String),
    Aborted(String),
    /// The host failed outside the script's control.
    Sandbox(String),
    /// A script error that `catch` does not stop: output or memory past the
    /// limit, or waiting on a promise nothing can settle. `name`, `message`.
    Script(String, String),
}

/// What runs the script's tool calls. The answers come in the order of the
/// calls: each the JSON text of the value, or the error message.
pub trait Host {
    fn call(&mut self, calls: Vec<HostCall>) -> Result<Vec<Result<String, String>>, Fatal>;
}

pub enum Flow {
    Throw(Value),
    Return(Value),
    Break(Option<Name>),
    Continue(Option<Name>),
    /// `?.` on a nullish value: the chain is `undefined`.
    Short,
    /// `exit()`: the script ends successfully.
    Exit,
    Fatal(Fatal),
}

pub type R<T> = Result<T, Flow>;

struct Binding {
    value: Value,
    mutable: bool,
}

pub struct Scope {
    vars: RefCell<HashMap<Rc<str>, Binding>>,
    parent: Option<Env>,
    /// Where `var` declares: a function's scope or the script's.
    function: bool,
}

pub type Env = Rc<Scope>;

impl Scope {
    pub fn new(parent: Option<Env>, function: bool) -> Env {
        let scope = Rc::new(Self {
            vars: RefCell::new(HashMap::new()),
            parent,
            function,
        });
        crate::heap::scope(&scope);
        scope
    }

    /// Every value the scope holds, taken out of it (`crate::heap`).
    pub fn take_values(&self) -> Vec<Value> {
        std::mem::take(&mut *self.vars.borrow_mut())
            .into_values()
            .map(|binding| binding.value)
            .collect()
    }

    /// Whether `name` is declared in this scope itself.
    pub fn has_own(&self, name: &str) -> bool {
        self.vars.borrow().contains_key(name)
    }

    pub fn declare(&self, name: Rc<str>, value: Value, mutable: bool) {
        self.vars
            .borrow_mut()
            .insert(name, Binding { value, mutable });
    }

    fn lookup(&self, name: &str) -> Option<Value> {
        if let Some(binding) = self.vars.borrow().get(name) {
            return Some(binding.value.clone());
        }
        self.parent.as_ref().and_then(|parent| parent.lookup(name))
    }

    /// `Ok(false)`: not declared anywhere. `Err(())`: a constant.
    fn assign(&self, name: &str, value: Value) -> Result<bool, ()> {
        if let Some(binding) = self.vars.borrow_mut().get_mut(name) {
            if !binding.mutable {
                return Err(());
            }
            binding.value = value;
            return Ok(true);
        }
        match &self.parent {
            Some(parent) => parent.assign(name, value),
            None => Ok(false),
        }
    }
}

/// What the script may use and how much of it.
pub struct Limits {
    pub deadline: Option<Instant>,
    pub cancel: Arc<AtomicBool>,
    /// Characters of output (`text()`, `console.*`) and items.
    pub max_output_chars: usize,
    pub max_output_items: usize,
    /// Roughly the bytes the script may allocate for strings, arrays and
    /// objects; past it the script fails as out of memory.
    pub memory_bytes: usize,
    /// Calls of script functions inside each other.
    pub max_depth: usize,
}

/// Steps between two looks at the clock and the cancel flag.
const POLL_EVERY: u64 = 256;
/// The longest array a script may make.
pub const MAX_ARRAY_LENGTH: usize = 50_000_000;
pub const MAX_STORE_VALUE_CHARS: usize = 256 * 1024;
pub const MAX_STORE_TOTAL_CHARS: usize = 1024 * 1024;

pub struct Interp<'h> {
    host: &'h mut dyn Host,
    limits: Limits,
    steps: u64,
    depth: usize,
    /// The line being run, for stack traces.
    pub line: u32,
    pub output: Vec<OutputItem>,
    output_chars: usize,
    pub calls: Vec<CallRecord>,
    /// Every tool call the script made, run or not.
    made: Vec<Rc<Promise>>,
    pub tools: Vec<ToolInfo>,
    store: IndexMap<String, String>,
    store_chars: usize,
    /// Keys `store()` set (`Some(json)`) or deleted (`None`), in order.
    pub writes: IndexMap<String, Option<String>>,
    allocated: usize,
    /// The script's scopes, kept until the run's values are let go of
    /// (`crate::heap::release`), so none is freed deep before.
    roots: Vec<Env>,
    random: u64,
}

pub enum Outcome {
    Returned(Value),
    Exited,
    Threw(Value),
    Fatal(Fatal),
}

impl<'h> Interp<'h> {
    pub fn new(
        host: &'h mut dyn Host,
        limits: Limits,
        tools: Vec<ToolInfo>,
        store: IndexMap<String, String>,
    ) -> Self {
        let store_chars = store.iter().map(|(key, json)| key.len() + json.len()).sum();
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9e37_79b9_7f4a_7c15, |elapsed| {
                u64::try_from(elapsed.as_nanos() & u128::from(u64::MAX)).unwrap_or(1)
            });
        Self {
            host,
            limits,
            steps: 0,
            depth: 0,
            line: 1,
            output: Vec::new(),
            output_chars: 0,
            calls: Vec::new(),
            made: Vec::new(),
            tools,
            store,
            store_chars,
            writes: IndexMap::new(),
            allocated: 0,
            roots: Vec::new(),
            random: seed | 1,
        }
    }

    /// Run the script and settle what it returned.
    pub fn run(&mut self, body: &[Stmt]) -> Outcome {
        let global = Scope::new(None, true);
        builtins::install_globals(self, &global);
        let script = Scope::new(Some(global), true);
        self.roots.push(Rc::clone(&script));
        hoist_vars(body, &script);
        let outcome = match self.exec_block(body, &script) {
            Ok(()) => Ok(Value::Undefined),
            Err(Flow::Return(value)) => Ok(value),
            Err(flow) => Err(flow),
        };
        // An async function's result adopts a returned promise.
        let outcome = outcome.and_then(|value| self.await_value(value));
        let outcome = match outcome {
            Ok(value) => Outcome::Returned(value),
            Err(Flow::Exit) => Outcome::Exited,
            Err(Flow::Throw(value)) => Outcome::Threw(value),
            Err(Flow::Fatal(fatal)) => Outcome::Fatal(fatal),
            Err(Flow::Break(_) | Flow::Continue(_)) => Outcome::Threw(
                self.error_value("SyntaxError", "Illegal break or continue statement"),
            ),
            Err(Flow::Short | Flow::Return(_)) => Outcome::Returned(Value::Undefined),
        };
        self.record_unrun();
        outcome
    }

    /// Calls nothing awaited: they never ran.
    fn record_unrun(&mut self) {
        for promise in std::mem::take(&mut self.made) {
            if matches!(
                &*promise.state.borrow(),
                PromiseState::Call { name: _, args: _ }
            ) {
                let name = match &*promise.state.borrow() {
                    PromiseState::Call { name, .. } => name.to_string(),
                    _ => String::new(),
                };
                self.calls.push(CallRecord {
                    name,
                    status: CallStatus::Cancelled,
                    duration_ms: 0,
                });
            }
        }
    }

    pub fn store_json(&self, key: &str) -> Option<&String> {
        self.store.get(key)
    }

    // ---- limits -----------------------------------------------------------

    pub fn tick(&mut self) -> R<()> {
        self.steps += 1;
        if self.steps.is_multiple_of(POLL_EVERY) {
            self.poll()?;
        }
        Ok(())
    }

    fn poll(&self) -> R<()> {
        if self.limits.cancel.load(Ordering::Relaxed) {
            return Err(Flow::Fatal(Fatal::Aborted("Execution aborted".to_owned())));
        }
        if let Some(deadline) = self.limits.deadline
            && Instant::now() >= deadline
        {
            return Err(Flow::Fatal(Fatal::Timeout(
                "Execution timed out".to_owned(),
            )));
        }
        Ok(())
    }

    /// Count `bytes` against the memory budget.
    pub fn alloc(&mut self, bytes: usize) -> R<()> {
        self.allocated = self.allocated.saturating_add(bytes);
        if self.allocated > self.limits.memory_bytes {
            return Err(Flow::Fatal(Fatal::Script(
                "InternalError".to_owned(),
                format!(
                    "out of memory: the script allocated more than {} MiB. Keep less in memory: filter tool results as you go, or write large data to a file with a tool.",
                    self.limits.memory_bytes / (1024 * 1024)
                ),
            )));
        }
        Ok(())
    }

    pub fn next_random(&mut self) -> f64 {
        // xorshift64*
        let mut x = self.random;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.random = x;
        let bits = x.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 11;
        bits as f64 / (1_u64 << 53) as f64
    }

    pub fn push_output(&mut self, text: String, console: bool) -> R<()> {
        self.output_chars = self.output_chars.saturating_add(text.len());
        if self.output_chars > self.limits.max_output_chars
            || self.output.len() >= self.limits.max_output_items
        {
            return Err(Flow::Fatal(Fatal::Script(
                "RangeError".to_owned(),
                format!(
                    "script output exceeded the limit of {} characters or {} text() and console calls. Print a summary instead, or write large data to a file with a tool.",
                    self.limits.max_output_chars, self.limits.max_output_items
                ),
            )));
        }
        self.output.push(OutputItem { text, console });
        Ok(())
    }

    // ---- store ------------------------------------------------------------

    pub fn store_set(&mut self, key: &str, json: Option<String>) -> R<()> {
        let previous = self.store.get(key).map_or(0, |old| key.len() + old.len());
        match json {
            None => {
                self.store.shift_remove(key);
                self.store_chars -= previous;
                self.writes.insert(key.to_owned(), None);
            }
            Some(json) => {
                const HINT: &str = "store() is for small state such as IDs or summaries. Keep large data in variables, or write it to a file with a tool.";
                if json.chars().count() > MAX_STORE_VALUE_CHARS {
                    return Err(self.throw(
                        "RangeError",
                        format!(
                            "store({}) value has {} characters of JSON, more than the limit of {MAX_STORE_VALUE_CHARS}. {HINT}",
                            quote(key),
                            json.chars().count()
                        ),
                    ));
                }
                let next = self.store_chars - previous + key.len() + json.len();
                if next > MAX_STORE_TOTAL_CHARS {
                    return Err(self.throw(
                        "RangeError",
                        format!(
                            "store is full: stored values would exceed {MAX_STORE_TOTAL_CHARS} characters of JSON. Delete keys with store(key, undefined). {HINT}"
                        ),
                    ));
                }
                self.store_chars = next;
                self.store.insert(key.to_owned(), json.clone());
                self.writes.insert(key.to_owned(), Some(json));
            }
        }
        Ok(())
    }

    // ---- errors -----------------------------------------------------------

    pub fn error_value(&self, name: &str, message: &str) -> Value {
        let stack = format!(
            "{}\n    at codemode.js:{}",
            if message.is_empty() {
                name.to_owned()
            } else {
                format!("{name}: {message}")
            },
            self.line
        );
        let mut props = IndexMap::new();
        props.insert(Rc::from("name"), Value::str(name));
        props.insert(Rc::from("message"), Value::str(message));
        props.insert(Rc::from("stack"), Value::str(stack));
        let error = Rc::new(Object {
            props: RefCell::new(props),
            class: Class::Error,
            frozen: std::cell::Cell::new(false),
        });
        crate::heap::object(&error);
        Value::Object(error)
    }

    pub fn throw(&self, name: &str, message: impl AsRef<str>) -> Flow {
        Flow::Throw(self.error_value(name, message.as_ref()))
    }

    pub fn type_error(&self, message: impl AsRef<str>) -> Flow {
        self.throw("TypeError", message)
    }

    // ---- statements -------------------------------------------------------

    pub fn exec_block(&mut self, body: &[Stmt], env: &Env) -> R<()> {
        // Function declarations are hoisted to the top of their block.
        for stmt in body {
            if let StmtKind::Function(name, def) = &stmt.kind {
                let function = Self::closure(def, env);
                env.declare(name.clone(), function, true);
            }
        }
        for stmt in body {
            self.exec(stmt, env)?;
        }
        Ok(())
    }

    fn closure(def: &Rc<FunctionDef>, env: &Env) -> Value {
        Value::Function(Rc::new(Function::Closure {
            def: Rc::clone(def),
            env: Rc::clone(env),
        }))
    }

    fn declare_var(env: &Env, kind: VarKind, name: Rc<str>, value: Value) {
        match kind {
            VarKind::Var => {
                let mut scope = Rc::clone(env);
                while !scope.function {
                    let Some(parent) = scope.parent.clone() else {
                        break;
                    };
                    scope = parent;
                }
                scope.declare(name, value, true);
            }
            VarKind::Let => env.declare(name, value, true),
            VarKind::Const => env.declare(name, value, false),
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one arm per statement kind, as the grammar lists them"
    )]
    fn exec(&mut self, stmt: &Stmt, env: &Env) -> R<()> {
        self.line = stmt.line;
        self.tick()?;
        match &stmt.kind {
            StmtKind::Empty | StmtKind::Function(..) => Ok(()),
            StmtKind::Var(kind, declarators) => {
                for declarator in declarators {
                    // `var x;` leaves an `x` that is already there as it is:
                    // every `var` was hoisted as `undefined`.
                    if *kind == VarKind::Var && declarator.init.is_none() {
                        continue;
                    }
                    let value = match &declarator.init {
                        Some(init) => self.eval(init, env)?,
                        None => Value::Undefined,
                    };
                    self.bind(&declarator.target, value, env, Some(*kind))?;
                }
                Ok(())
            }
            StmtKind::Expr(expr) => self.eval(expr, env).map(|_| ()),
            StmtKind::If(test, then, otherwise) => {
                if self.eval(test, env)?.truthy() {
                    self.exec_scoped(then, env)
                } else if let Some(otherwise) = otherwise {
                    self.exec_scoped(otherwise, env)
                } else {
                    Ok(())
                }
            }
            StmtKind::Block(body) => {
                let scope = Scope::new(Some(Rc::clone(env)), false);
                self.exec_block(body, &scope)
            }
            StmtKind::Return(value) => {
                let value = match value {
                    Some(expr) => self.eval(expr, env)?,
                    None => Value::Undefined,
                };
                Err(Flow::Return(value))
            }
            StmtKind::Break(label) => Err(Flow::Break(label.clone())),
            StmtKind::Continue(label) => Err(Flow::Continue(label.clone())),
            StmtKind::Throw(expr) => {
                let value = self.eval(expr, env)?;
                Err(Flow::Throw(value))
            }
            StmtKind::Try {
                block,
                param,
                handler,
                finalizer,
            } => {
                let scope = Scope::new(Some(Rc::clone(env)), false);
                let mut result = self.exec_block(block, &scope);
                if let (Err(Flow::Throw(thrown)), Some(handler)) = (&result, handler) {
                    let thrown = thrown.clone();
                    let scope = Scope::new(Some(Rc::clone(env)), false);
                    result = match param {
                        Some(param) => self.bind(param, thrown, &scope, Some(VarKind::Let)),
                        None => Ok(()),
                    }
                    .and_then(|()| self.exec_block(handler, &scope));
                }
                if let Some(finalizer) = finalizer {
                    // What cannot be caught does not run `finally` either: it
                    // stops the script now.
                    if matches!(result, Err(Flow::Fatal(_) | Flow::Exit)) {
                        return result;
                    }
                    let scope = Scope::new(Some(Rc::clone(env)), false);
                    self.exec_block(finalizer, &scope)?;
                }
                result
            }
            StmtKind::Labeled(label, body) => {
                let mut labels = vec![label.clone()];
                let mut inner = &**body;
                while let StmtKind::Labeled(label, body) = &inner.kind {
                    labels.push(label.clone());
                    inner = body;
                }
                match self.exec_loop(inner, env, &labels) {
                    Err(Flow::Break(Some(label))) if labels.contains(&label) => Ok(()),
                    other => other,
                }
            }
            StmtKind::Switch(discriminant, cases) => {
                let value = self.eval(discriminant, env)?;
                let scope = Scope::new(Some(Rc::clone(env)), false);
                let mut matched = None;
                for (index, case) in cases.iter().enumerate() {
                    if let Some(test) = &case.test {
                        let test = self.eval(test, &scope)?;
                        if strict_equals(&value, &test) {
                            matched = Some(index);
                            break;
                        }
                    }
                }
                let start = matched.or_else(|| cases.iter().position(|case| case.test.is_none()));
                if let Some(start) = start {
                    for case in &cases[start..] {
                        match self.exec_block(&case.body, &scope) {
                            Err(Flow::Break(None)) => return Ok(()),
                            other => other?,
                        }
                    }
                }
                Ok(())
            }
            StmtKind::For { .. }
            | StmtKind::ForEach { .. }
            | StmtKind::While(..)
            | StmtKind::DoWhile(..) => self.exec_loop(stmt, env, &[]),
        }
    }

    /// A statement in a position of its own (`if` branch, loop body): a
    /// declaration there gets a scope of its own.
    fn exec_scoped(&mut self, stmt: &Stmt, env: &Env) -> R<()> {
        if matches!(stmt.kind, StmtKind::Var(..) | StmtKind::Function(..)) {
            let scope = Scope::new(Some(Rc::clone(env)), false);
            if let StmtKind::Function(name, def) = &stmt.kind {
                let function = Self::closure(def, &scope);
                scope.declare(name.clone(), function, true);
            }
            self.exec(stmt, &scope)
        } else {
            self.exec(stmt, env)
        }
    }

    /// Whether a loop goes on after its body ended with `flow`.
    fn loop_flow(flow: R<()>, labels: &[Name]) -> R<bool> {
        match flow {
            Ok(()) | Err(Flow::Continue(None)) => Ok(true),
            Err(Flow::Continue(Some(label))) if labels.contains(&label) => Ok(true),
            Err(Flow::Break(None)) => Ok(false),
            Err(Flow::Break(Some(label))) if labels.contains(&label) => Ok(false),
            Err(flow) => Err(flow),
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the four loops share their label and flow handling"
    )]
    fn exec_loop(&mut self, stmt: &Stmt, env: &Env, labels: &[Name]) -> R<()> {
        self.line = stmt.line;
        match &stmt.kind {
            StmtKind::While(test, body) => {
                while self.eval(test, env)?.truthy() {
                    self.tick()?;
                    if !Self::loop_flow(self.exec_scoped(body, env), labels)? {
                        break;
                    }
                }
                Ok(())
            }
            StmtKind::DoWhile(body, test) => {
                loop {
                    self.tick()?;
                    if !Self::loop_flow(self.exec_scoped(body, env), labels)? {
                        break;
                    }
                    if !self.eval(test, env)?.truthy() {
                        break;
                    }
                }
                Ok(())
            }
            StmtKind::For {
                init,
                test,
                update,
                body,
            } => {
                let mut scope = Scope::new(Some(Rc::clone(env)), false);
                let mut per_iteration: Vec<Rc<str>> = Vec::new();
                match init {
                    Some(ForInit::Var(kind, declarators)) => {
                        for declarator in declarators {
                            let value = match &declarator.init {
                                Some(init) => self.eval(init, &scope)?,
                                None => Value::Undefined,
                            };
                            self.bind(&declarator.target, value, &scope, Some(*kind))?;
                            if *kind == VarKind::Let {
                                pattern_names(&declarator.target, &mut per_iteration);
                            }
                        }
                    }
                    Some(ForInit::Expr(expr)) => {
                        self.eval(expr, &scope)?;
                    }
                    None => {}
                }
                loop {
                    self.tick()?;
                    if let Some(test) = test
                        && !self.eval(test, &scope)?.truthy()
                    {
                        break;
                    }
                    if !Self::loop_flow(self.exec_scoped(body, &scope), labels)? {
                        break;
                    }
                    // `let` bindings are fresh each iteration, so a closure
                    // made in the body keeps the value it saw.
                    if !per_iteration.is_empty() {
                        let next = Scope::new(Some(Rc::clone(env)), false);
                        for name in &per_iteration {
                            let value = scope.lookup(name).unwrap_or(Value::Undefined);
                            next.declare(name.clone(), value, true);
                        }
                        scope = next;
                    }
                    if let Some(update) = update {
                        self.eval(update, &scope)?;
                    }
                }
                Ok(())
            }
            StmtKind::ForEach {
                head,
                iterable,
                body,
                keys,
            } => {
                let source = self.eval(iterable, env)?;
                // An array is read as the loop goes, as JavaScript's array
                // iterator reads it: an item pushed in the body is visited.
                let live = match (&source, *keys) {
                    (Value::Array(array), false) => Some(Rc::clone(array)),
                    _ => None,
                };
                let mut items = match &live {
                    Some(_) => Vec::new().into_iter(),
                    None if *keys => builtins::for_in_keys(&source).into_iter(),
                    None => self.iterate(&source)?.into_iter(),
                };
                let mut position = 0_usize;
                loop {
                    let item = match &live {
                        Some(array) => {
                            let next = array.items.borrow().get(position).cloned();
                            position += 1;
                            match next {
                                Some(item) => item,
                                None => break,
                            }
                        }
                        None => match items.next() {
                            Some(item) => item,
                            None => break,
                        },
                    };
                    self.tick()?;
                    let scope = Scope::new(Some(Rc::clone(env)), false);
                    match head {
                        ForHead::Var(kind, target) => {
                            self.bind(target, item, &scope, Some(*kind))?;
                        }
                        ForHead::Target(target) => {
                            self.bind(target, item, &scope, None)?;
                        }
                    }
                    if !Self::loop_flow(self.exec_scoped(body, &scope), labels)? {
                        break;
                    }
                }
                Ok(())
            }
            _ => self.exec(stmt, env),
        }
    }

    // ---- patterns ---------------------------------------------------------

    /// Bind `value` to `target`: declare it when `kind` is given, assign it
    /// otherwise.
    pub fn bind(
        &mut self,
        target: &Pattern,
        value: Value,
        env: &Env,
        kind: Option<VarKind>,
    ) -> R<()> {
        match target {
            Pattern::Ident(name) => match kind {
                Some(kind) => {
                    Self::declare_var(env, kind, name.clone(), value);
                    Ok(())
                }
                None => self.assign_name(name, value, env),
            },
            Pattern::Member(expr) => {
                let Expr::Member { object, key, .. } = &**expr else {
                    return Err(self.throw("SyntaxError", "Invalid assignment target"));
                };
                let object = self.eval(object, env)?;
                let key = self.member_key(key, env)?;
                builtins::set_property(self, &object, &key, value)
            }
            Pattern::Array { items, rest } => {
                let values = self.iterate(&value)?;
                for (index, item) in items.iter().enumerate() {
                    let Some(item) = item else { continue };
                    let mut element = values.get(index).cloned().unwrap_or(Value::Undefined);
                    if matches!(element, Value::Undefined)
                        && let Some(default) = &item.default
                    {
                        element = self.eval(default, env)?;
                    }
                    self.bind(&item.target, element, env, kind)?;
                }
                if let Some(rest) = rest {
                    let remaining = values.get(items.len()..).unwrap_or_default().to_vec();
                    self.bind(rest, Value::array(remaining), env, kind)?;
                }
                Ok(())
            }
            Pattern::Object { props, rest } => {
                if value.is_nullish() {
                    return Err(self.type_error(format!(
                        "Cannot destructure '{}' as it is {}.",
                        to_string(&value),
                        to_string(&value)
                    )));
                }
                let mut used: Vec<Rc<str>> = Vec::new();
                for prop in props {
                    let key = match &prop.key {
                        PropKey::Name(name) => name.clone(),
                        PropKey::Computed(expr) => {
                            let key = self.eval(expr, env)?;
                            to_key(&key)
                        }
                    };
                    let mut element =
                        builtins::get_property(self, &value, &Value::Str(key.clone()))?;
                    used.push(key);
                    if matches!(element, Value::Undefined)
                        && let Some(default) = &prop.default
                    {
                        element = self.eval(default, env)?;
                    }
                    self.bind(&prop.target, element, env, kind)?;
                }
                if let Some(rest) = rest {
                    let mut remaining = IndexMap::new();
                    for (key, item) in builtins::own_entries(&value) {
                        if !used.contains(&key) {
                            remaining.insert(key, item);
                        }
                    }
                    let remaining = Value::object(remaining);
                    match kind {
                        Some(kind) => Self::declare_var(env, kind, rest.clone(), remaining),
                        None => self.assign_name(rest, remaining, env)?,
                    }
                }
                Ok(())
            }
        }
    }

    fn assign_name(&mut self, name: &Rc<str>, value: Value, env: &Env) -> R<()> {
        match env.assign(name, value) {
            Ok(true) => Ok(()),
            Ok(false) => Err(self.throw("ReferenceError", format!("{name} is not defined"))),
            Err(()) => Err(self.type_error("Assignment to constant variable.")),
        }
    }

    // ---- expressions ------------------------------------------------------

    pub fn member_key(&mut self, key: &MemberKey, env: &Env) -> R<Value> {
        match key {
            MemberKey::Name(name) => Ok(Value::Str(name.clone())),
            MemberKey::Computed(expr) => self.eval(expr, env),
        }
    }

    pub fn lookup(&mut self, name: &Rc<str>, env: &Env) -> R<Value> {
        if let Some(value) = env.lookup(name) {
            return Ok(value);
        }
        match builtins::global(name) {
            Some(value) => Ok(value),
            None => Err(self.throw("ReferenceError", format!("{name} is not defined"))),
        }
    }

    fn spread_into(&mut self, elements: &[Element], env: &Env) -> R<Vec<Value>> {
        let mut values = Vec::with_capacity(elements.len());
        for element in elements {
            match element {
                Element::Expr(expr) => values.push(self.eval(expr, env)?),
                Element::Spread(expr) => {
                    let source = self.eval(expr, env)?;
                    values.extend(self.iterate(&source)?);
                }
                Element::Hole => values.push(Value::Undefined),
            }
        }
        self.alloc(values.len() * 16)?;
        Ok(values)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one arm per expression kind, as the grammar lists them"
    )]
    pub fn eval(&mut self, expr: &Expr, env: &Env) -> R<Value> {
        match expr {
            Expr::Number(number) => Ok(Value::Number(*number)),
            Expr::Str(text) => Ok(Value::Str(text.clone())),
            Expr::Bool(value) => Ok(Value::Bool(*value)),
            Expr::Null => Ok(Value::Null),
            Expr::This => Ok(env.lookup("this").unwrap_or(Value::Undefined)),
            Expr::Ident(name) => self.lookup(name, env),
            Expr::Template(quasis, exprs) => {
                let mut text = String::from(&*quasis[0]);
                for (index, expr) in exprs.iter().enumerate() {
                    let value = self.eval(expr, env)?;
                    text.push_str(&builtins::to_display_string(&value));
                    text.push_str(&quasis[index + 1]);
                }
                self.alloc(text.len())?;
                Ok(Value::str(text))
            }
            Expr::Regex(pattern, flags) => builtins::make_regex(self, pattern, flags),
            Expr::Array(elements) => Ok(Value::array(self.spread_into(elements, env)?)),
            Expr::Object(properties) => {
                let mut props: IndexMap<Rc<str>, Value> = IndexMap::new();
                for property in properties {
                    match property {
                        Property::Value(key, value) => {
                            let key = match key {
                                PropKey::Name(name) => name.clone(),
                                PropKey::Computed(expr) => {
                                    let key = self.eval(expr, env)?;
                                    to_key(&key)
                                }
                            };
                            let value = self.eval(value, env)?;
                            props.insert(key, value);
                        }
                        Property::Shorthand(name) => {
                            let value = self.lookup(name, env)?;
                            props.insert(name.clone(), value);
                        }
                        Property::Spread(expr) => {
                            let source = self.eval(expr, env)?;
                            for (key, value) in builtins::own_entries(&source) {
                                props.insert(key, value);
                            }
                        }
                    }
                }
                self.alloc(props.len() * 32)?;
                Ok(Value::object(props))
            }
            Expr::Function(def) => Ok(Self::closure(def, env)),
            Expr::Unary(op, operand) => self.unary(*op, operand, env),
            Expr::Update {
                increment,
                prefix,
                target,
            } => {
                let step = |old: f64| if *increment { old + 1.0 } else { old - 1.0 };
                // The target's object and key are evaluated once: `a[i++]++`.
                let (old, new) = if let Expr::Member { object, key, .. } = &**target {
                    let object = self.eval(object, env)?;
                    let key = self.member_key(key, env)?;
                    let old = to_number(&builtins::get_property(self, &object, &key)?);
                    let new = step(old);
                    builtins::set_property(self, &object, &key, Value::Number(new))?;
                    (old, new)
                } else {
                    let old = to_number(&self.eval(target, env)?);
                    let new = step(old);
                    self.assign_to(target, Value::Number(new), env)?;
                    (old, new)
                };
                Ok(Value::Number(if *prefix { new } else { old }))
            }
            Expr::Binary(op, left, right) => {
                let left = self.eval(left, env)?;
                let right = self.eval(right, env)?;
                self.binary(*op, &left, &right)
            }
            Expr::Logical(op, left, right) => {
                let left = self.eval(left, env)?;
                let take_left = match op {
                    LogicalOp::And => !left.truthy(),
                    LogicalOp::Or => left.truthy(),
                    LogicalOp::Nullish => !left.is_nullish(),
                };
                if take_left {
                    Ok(left)
                } else {
                    self.eval(right, env)
                }
            }
            Expr::Assign(op, target, value) => self.assign(*op, target, value, env),
            Expr::Conditional(test, then, otherwise) => {
                if self.eval(test, env)?.truthy() {
                    self.eval(then, env)
                } else {
                    self.eval(otherwise, env)
                }
            }
            Expr::Call {
                callee,
                args,
                optional,
            } => {
                let (function, this) = match &**callee {
                    Expr::Member {
                        object,
                        key,
                        optional: member_optional,
                    } => {
                        let object = self.eval(object, env)?;
                        if *member_optional && object.is_nullish() {
                            return Err(Flow::Short);
                        }
                        let key = self.member_key(key, env)?;
                        let function = builtins::get_property(self, &object, &key)?;
                        (function, object)
                    }
                    Expr::Ident(name) if &**name == "__forAwait" => {
                        let values = self.spread_into(args, env)?;
                        let items = self.iterate(values.first().unwrap_or(&Value::Undefined))?;
                        let mut settled = Vec::with_capacity(items.len());
                        for item in items {
                            settled.push(self.await_value(item)?);
                        }
                        return Ok(Value::array(settled));
                    }
                    other => (self.eval(other, env)?, Value::Undefined),
                };
                if *optional && function.is_nullish() {
                    return Err(Flow::Short);
                }
                if !matches!(function, Value::Function(_)) {
                    return Err(
                        self.type_error(format!("{} is not a function", describe_callee(callee)))
                    );
                }
                let args = self.spread_into(args, env)?;
                self.call(&function, this, args)
            }
            Expr::New(callee, args) => {
                let constructor = self.eval(callee, env)?;
                let args = self.spread_into(args, env)?;
                self.construct(&constructor, args, callee)
            }
            Expr::Member {
                object,
                key,
                optional,
            } => {
                let object = self.eval(object, env)?;
                if *optional && object.is_nullish() {
                    return Err(Flow::Short);
                }
                let key = self.member_key(key, env)?;
                builtins::get_property(self, &object, &key)
            }
            Expr::Chain(inner) => match self.eval(inner, env) {
                Err(Flow::Short) => Ok(Value::Undefined),
                other => other,
            },
            Expr::Sequence(items) => {
                let mut last = Value::Undefined;
                for item in items {
                    last = self.eval(item, env)?;
                }
                Ok(last)
            }
            Expr::Await(operand) => {
                let value = self.eval(operand, env)?;
                self.await_value(value)
            }
        }
    }

    fn unary(&mut self, op: UnaryOp, operand: &Expr, env: &Env) -> R<Value> {
        match op {
            UnaryOp::Typeof => {
                // `typeof undeclared` is "undefined", not an error.
                if let Expr::Ident(name) = operand
                    && env.lookup(name).is_none()
                    && builtins::global(name).is_none()
                {
                    return Ok(Value::str("undefined"));
                }
                let value = self.eval(operand, env)?;
                Ok(Value::str(value.type_of()))
            }
            UnaryOp::Delete => {
                if let Expr::Member { object, key, .. } = operand {
                    let object = self.eval(object, env)?;
                    let key = self.member_key(key, env)?;
                    builtins::delete_property(self, &object, &key)?;
                }
                Ok(Value::Bool(true))
            }
            UnaryOp::Void => {
                self.eval(operand, env)?;
                Ok(Value::Undefined)
            }
            UnaryOp::Not => Ok(Value::Bool(!self.eval(operand, env)?.truthy())),
            UnaryOp::Neg => Ok(Value::Number(-to_number(&self.eval(operand, env)?))),
            UnaryOp::Plus => Ok(Value::Number(to_number(&self.eval(operand, env)?))),
            UnaryOp::BitNot => Ok(Value::Number(f64::from(!to_int32(to_number(
                &self.eval(operand, env)?,
            ))))),
        }
    }

    fn assign_to(&mut self, target: &Expr, value: Value, env: &Env) -> R<()> {
        match target {
            Expr::Ident(name) => self.assign_name(name, value, env),
            Expr::Member { object, key, .. } => {
                let object = self.eval(object, env)?;
                let key = self.member_key(key, env)?;
                builtins::set_property(self, &object, &key, value)
            }
            _ => Err(self.throw("SyntaxError", "Invalid left-hand side in assignment")),
        }
    }

    fn assign(&mut self, op: AssignOp, target: &Pattern, value: &Expr, env: &Env) -> R<Value> {
        if op == AssignOp::Assign {
            let value = self.eval(value, env)?;
            self.bind(target, value.clone(), env, None)?;
            return Ok(value);
        }
        // Compound assignment reads the target once: `obj[f()] += 1` calls
        // `f` once.
        let (current, object_key) = match target {
            Pattern::Ident(name) => (self.lookup(name, env)?, None),
            Pattern::Member(expr) => {
                let Expr::Member { object, key, .. } = &**expr else {
                    return Err(self.throw("SyntaxError", "Invalid assignment target"));
                };
                let object = self.eval(object, env)?;
                let key = self.member_key(key, env)?;
                let current = builtins::get_property(self, &object, &key)?;
                (current, Some((object, key)))
            }
            _ => return Err(self.throw("SyntaxError", "Invalid assignment target")),
        };
        let new = match op {
            AssignOp::Binary(op) => {
                let right = self.eval(value, env)?;
                self.binary(op, &current, &right)?
            }
            AssignOp::Logical(op) => {
                let keep = match op {
                    LogicalOp::And => !current.truthy(),
                    LogicalOp::Or => current.truthy(),
                    LogicalOp::Nullish => !current.is_nullish(),
                };
                if keep {
                    return Ok(current);
                }
                self.eval(value, env)?
            }
            AssignOp::Assign => unreachable!("handled above"),
        };
        match (target, object_key) {
            (Pattern::Ident(name), _) => self.assign_name(name, new.clone(), env)?,
            (_, Some((object, key))) => builtins::set_property(self, &object, &key, new.clone())?,
            _ => {}
        }
        Ok(new)
    }

    pub fn binary(&mut self, op: BinaryOp, left: &Value, right: &Value) -> R<Value> {
        let number = |value: f64| Ok(Value::Number(value));
        match op {
            BinaryOp::Add => {
                let left = builtins::to_primitive(left);
                let right = builtins::to_primitive(right);
                if matches!(left, Value::Str(_)) || matches!(right, Value::Str(_)) {
                    let mut text = to_string(&left);
                    let tail = to_string(&right);
                    self.alloc(tail.len())?;
                    text.push_str(&tail);
                    Ok(Value::str(text))
                } else {
                    number(to_number(&left) + to_number(&right))
                }
            }
            BinaryOp::Sub => number(to_number(left) - to_number(right)),
            BinaryOp::Mul => number(to_number(left) * to_number(right)),
            BinaryOp::Div => number(to_number(left) / to_number(right)),
            BinaryOp::Rem => number(js_remainder(to_number(left), to_number(right))),
            BinaryOp::Pow => number(js_pow(to_number(left), to_number(right))),
            BinaryOp::StrictEq => Ok(Value::Bool(strict_equals(left, right))),
            BinaryOp::StrictNotEq => Ok(Value::Bool(!strict_equals(left, right))),
            BinaryOp::Eq => Ok(Value::Bool(loose_equals(left, right))),
            BinaryOp::NotEq => Ok(Value::Bool(!loose_equals(left, right))),
            BinaryOp::Lt | BinaryOp::Gt | BinaryOp::LtEq | BinaryOp::GtEq => {
                let left = builtins::to_primitive(left);
                let right = builtins::to_primitive(right);
                let ordering = if let (Value::Str(a), Value::Str(b)) = (&left, &right) {
                    Some(a.cmp(b))
                } else {
                    to_number(&left).partial_cmp(&to_number(&right))
                };
                let result = ordering.is_some_and(|ordering| match op {
                    BinaryOp::Lt => ordering.is_lt(),
                    BinaryOp::Gt => ordering.is_gt(),
                    BinaryOp::LtEq => ordering.is_le(),
                    _ => ordering.is_ge(),
                });
                Ok(Value::Bool(result))
            }
            BinaryOp::BitAnd => number(f64::from(
                to_int32(to_number(left)) & to_int32(to_number(right)),
            )),
            BinaryOp::BitOr => number(f64::from(
                to_int32(to_number(left)) | to_int32(to_number(right)),
            )),
            BinaryOp::BitXor => number(f64::from(
                to_int32(to_number(left)) ^ to_int32(to_number(right)),
            )),
            BinaryOp::Shl => number(f64::from(
                to_int32(to_number(left)).wrapping_shl(to_uint32(to_number(right)) & 31),
            )),
            BinaryOp::Shr => number(f64::from(
                to_int32(to_number(left)).wrapping_shr(to_uint32(to_number(right)) & 31),
            )),
            BinaryOp::UShr => number(f64::from(
                to_uint32(to_number(left)).wrapping_shr(to_uint32(to_number(right)) & 31),
            )),
            BinaryOp::In => {
                if !matches!(
                    right,
                    Value::Object(_)
                        | Value::Array(_)
                        | Value::Tools
                        | Value::Map(_)
                        | Value::Set(_)
                        | Value::Function(_)
                        | Value::Namespace(_)
                ) {
                    return Err(self.type_error(format!(
                        "Cannot use 'in' operator to search for '{}' in {}",
                        to_string(left),
                        to_string(right)
                    )));
                }
                Ok(Value::Bool(builtins::has_property(
                    self,
                    right,
                    &to_key(left),
                )))
            }
            BinaryOp::InstanceOf => Ok(Value::Bool(builtins::instance_of(left, right))),
        }
    }

    // ---- calls ------------------------------------------------------------

    pub fn call(&mut self, function: &Value, this: Value, args: Vec<Value>) -> R<Value> {
        let Value::Function(function) = function else {
            return Err(self.type_error(format!("{} is not a function", to_string(function))));
        };
        self.tick()?;
        match &**function {
            Function::Native { name, this: bound } => {
                let bound = if matches!(bound, Value::Undefined) {
                    this
                } else {
                    bound.clone()
                };
                let name = name.clone();
                builtins::call_native(self, &name, bound, args)
            }
            Function::Closure { def, env } => {
                let def = Rc::clone(def);
                let env = Rc::clone(env);
                self.call_closure(&def, &env, this, args)
            }
        }
    }

    fn call_closure(
        &mut self,
        def: &Rc<FunctionDef>,
        env: &Env,
        this: Value,
        args: Vec<Value>,
    ) -> R<Value> {
        if self.depth >= self.limits.max_depth {
            return Err(self.throw("RangeError", "Maximum call stack size exceeded"));
        }
        self.depth += 1;
        let line = self.line;
        let result = self.call_body(def, env, this, args);
        self.depth -= 1;
        self.line = line;
        if def.is_async {
            return match result {
                Ok(Value::Promise(promise)) => Ok(Value::Promise(promise)),
                Ok(value) => Ok(Value::Promise(Promise::new(PromiseState::Settled(
                    Settled::Fulfilled(value),
                )))),
                Err(Flow::Throw(error)) => Ok(Value::Promise(Promise::new(PromiseState::Settled(
                    Settled::Rejected(error),
                )))),
                Err(flow) => Err(flow),
            };
        }
        result
    }

    fn call_body(
        &mut self,
        def: &Rc<FunctionDef>,
        env: &Env,
        this: Value,
        args: Vec<Value>,
    ) -> R<Value> {
        let scope = Scope::new(Some(Rc::clone(env)), true);
        if !def.arrow {
            scope.declare(Rc::from("this"), this, false);
            scope.declare(Rc::from("arguments"), Value::array(args.clone()), true);
        }
        if let Some(name) = &def.name
            && !def.arrow
        {
            // A named function expression sees itself by name.
            if scope.lookup(name).is_none() {
                scope.declare(
                    name.clone(),
                    Value::Function(Rc::new(Function::Closure {
                        def: Rc::clone(def),
                        env: Rc::clone(env),
                    })),
                    true,
                );
            }
        }
        let mut args = args.into_iter();
        for param in &def.params {
            let mut value = args.next().unwrap_or(Value::Undefined);
            if matches!(value, Value::Undefined)
                && let Some(default) = &param.default
            {
                value = self.eval(default, &scope)?;
            }
            self.bind(&param.target, value, &scope, Some(VarKind::Let))?;
        }
        if let Some(rest) = &def.rest {
            self.bind(
                rest,
                Value::array(args.collect()),
                &scope,
                Some(VarKind::Let),
            )?;
        }
        if let FunctionBody::Block(body) = &def.body {
            hoist_vars(body, &scope);
        }
        match &def.body {
            FunctionBody::Expr(expr) => self.eval(expr, &scope),
            FunctionBody::Block(body) => match self.exec_block(body, &scope) {
                Ok(()) => Ok(Value::Undefined),
                Err(Flow::Return(value)) => Ok(value),
                Err(flow) => Err(flow),
            },
        }
    }

    fn construct(&mut self, constructor: &Value, args: Vec<Value>, callee: &Expr) -> R<Value> {
        match constructor {
            Value::Function(function) => {
                match &**function {
                    Function::Native { name, .. } => {
                        let name = name.clone();
                        builtins::construct_native(self, &name, args)
                    }
                    Function::Closure { def, env } if !def.arrow && !def.is_async => {
                        let object = Value::object(IndexMap::new());
                        let def = Rc::clone(def);
                        let env = Rc::clone(env);
                        let returned = self.call_closure(&def, &env, object.clone(), args)?;
                        Ok(match returned {
                            Value::Object(_) | Value::Array(_) => returned,
                            _ => object,
                        })
                    }
                    Function::Closure { .. } => Err(self
                        .type_error(format!("{} is not a constructor", describe_callee(callee)))),
                }
            }
            _ => Err(self.type_error(format!("{} is not a constructor", describe_callee(callee)))),
        }
    }

    /// The items `for...of`, spread and array patterns go through.
    pub fn iterate(&mut self, value: &Value) -> R<Vec<Value>> {
        match builtins::iterate(value) {
            Some(items) => {
                self.alloc(items.len() * 16)?;
                Ok(items)
            }
            None => Err(self.type_error(format!("{} is not iterable", builtins::describe(value)))),
        }
    }

    // ---- promises ---------------------------------------------------------

    /// A new pending tool call.
    pub fn tool_call(&mut self, name: Rc<str>, args: Option<String>) -> Value {
        let promise = Promise::new(PromiseState::Call { name, args });
        self.made.push(Rc::clone(&promise));
        Value::Promise(promise)
    }

    /// `await value`.
    pub fn await_value(&mut self, value: Value) -> R<Value> {
        let Value::Promise(promise) = value else {
            return Ok(value);
        };
        match self.force(&promise)? {
            Settled::Fulfilled(value) => Ok(value),
            Settled::Rejected(error) => Err(Flow::Throw(error)),
        }
    }

    /// The tool calls `value` waits on that have not run yet.
    fn pending_calls(value: &Value, out: &mut Vec<Rc<Promise>>) {
        let Value::Promise(promise) = value else {
            return;
        };
        match &*promise.state.borrow() {
            PromiseState::Call { .. } => {
                if !out.iter().any(|seen| Rc::ptr_eq(seen, promise)) {
                    out.push(Rc::clone(promise));
                }
            }
            PromiseState::Then { base, .. } => {
                Self::pending_calls(&Value::Promise(Rc::clone(base)), out);
            }
            PromiseState::Combine { items, .. } => {
                for item in items {
                    Self::pending_calls(item, out);
                }
            }
            _ => {}
        }
    }

    fn stalled() -> Flow {
        Flow::Fatal(Fatal::Script(
            "Error".to_owned(),
            "The script is waiting on a promise that can never settle: no tool call is pending, and timers do not exist here.".to_owned(),
        ))
    }

    fn settle(promise: &Promise, settled: Settled) -> Settled {
        *promise.state.borrow_mut() = PromiseState::Settled(settled.clone());
        settled
    }

    pub fn force(&mut self, promise: &Rc<Promise>) -> R<Settled> {
        self.tick()?;
        let state = std::mem::replace(&mut *promise.state.borrow_mut(), PromiseState::Running);
        match state {
            PromiseState::Settled(settled) => Ok(Self::settle(promise, settled)),
            PromiseState::Waiting | PromiseState::Running => {
                *promise.state.borrow_mut() = PromiseState::Waiting;
                Err(Self::stalled())
            }
            state @ PromiseState::Call { .. } => {
                *promise.state.borrow_mut() = state;
                self.run_calls(std::slice::from_ref(promise))?;
                promise.settled().ok_or_else(Self::stalled)
            }
            PromiseState::Then {
                base,
                on_fulfilled,
                on_rejected,
                finally,
            } => {
                let outcome = self.force(&base)?;
                let settled = if finally {
                    match on_fulfilled {
                        Some(handler) => match self.call(&handler, Value::Undefined, Vec::new()) {
                            Ok(result) => match self.await_value(result) {
                                Ok(_) => outcome,
                                Err(Flow::Throw(error)) => Settled::Rejected(error),
                                Err(flow) => return Err(flow),
                            },
                            Err(Flow::Throw(error)) => Settled::Rejected(error),
                            Err(flow) => return Err(flow),
                        },
                        None => outcome,
                    }
                } else {
                    let (handler, argument) = match &outcome {
                        Settled::Fulfilled(value) => (on_fulfilled, value.clone()),
                        Settled::Rejected(error) => (on_rejected, error.clone()),
                    };
                    match handler {
                        Some(handler) if matches!(handler, Value::Function(_)) => {
                            match self.call(&handler, Value::Undefined, vec![argument]) {
                                Ok(result) => match self.await_value(result) {
                                    Ok(value) => Settled::Fulfilled(value),
                                    Err(Flow::Throw(error)) => Settled::Rejected(error),
                                    Err(flow) => return Err(flow),
                                },
                                Err(Flow::Throw(error)) => Settled::Rejected(error),
                                Err(flow) => return Err(flow),
                            }
                        }
                        _ => outcome,
                    }
                };
                Ok(Self::settle(promise, settled))
            }
            PromiseState::Combine { items, how } => {
                let mut calls = Vec::new();
                for item in &items {
                    Self::pending_calls(item, &mut calls);
                }
                if !calls.is_empty() {
                    self.run_calls(&calls)?;
                }
                let mut results = Vec::with_capacity(items.len());
                for item in &items {
                    results.push(match item {
                        Value::Promise(inner) => self.force(inner)?,
                        other => Settled::Fulfilled(other.clone()),
                    });
                }
                let settled = self.combine(how, results);
                Ok(Self::settle(promise, settled))
            }
        }
    }

    fn combine(&self, how: Combine, results: Vec<Settled>) -> Settled {
        match how {
            Combine::All => {
                let mut values = Vec::with_capacity(results.len());
                for result in results {
                    match result {
                        Settled::Fulfilled(value) => values.push(value),
                        rejected @ Settled::Rejected(_) => return rejected,
                    }
                }
                Settled::Fulfilled(Value::array(values))
            }
            Combine::AllSettled => Settled::Fulfilled(Value::array(
                results
                    .into_iter()
                    .map(|result| match result {
                        Settled::Fulfilled(value) => Value::object_from([
                            ("status", Value::str("fulfilled")),
                            ("value", value),
                        ]),
                        Settled::Rejected(reason) => Value::object_from([
                            ("status", Value::str("rejected")),
                            ("reason", reason),
                        ]),
                    })
                    .collect(),
            )),
            Combine::Race => results.into_iter().next().unwrap_or_else(|| {
                Settled::Rejected(
                    self.error_value("Error", "Promise.race of nothing never settles"),
                )
            }),
            Combine::Any => {
                let mut errors = Vec::new();
                for result in results {
                    match result {
                        fulfilled @ Settled::Fulfilled(_) => return fulfilled,
                        Settled::Rejected(error) => errors.push(error),
                    }
                }
                let error = self.error_value("AggregateError", "All promises were rejected");
                if let Value::Object(object) = &error {
                    object
                        .props
                        .borrow_mut()
                        .insert(Rc::from("errors"), Value::array(errors));
                }
                Settled::Rejected(error)
            }
        }
    }

    /// Hand these calls to the host at once and settle each with its answer.
    fn run_calls(&mut self, promises: &[Rc<Promise>]) -> R<()> {
        self.poll()?;
        let mut batch = Vec::with_capacity(promises.len());
        for promise in promises {
            let state = std::mem::replace(&mut *promise.state.borrow_mut(), PromiseState::Running);
            let PromiseState::Call { name, args } = state else {
                *promise.state.borrow_mut() = state;
                continue;
            };
            promise.call.set(Some(self.calls.len()));
            self.calls.push(CallRecord {
                name: name.to_string(),
                status: CallStatus::Cancelled,
                duration_ms: 0,
            });
            batch.push(HostCall {
                name: name.to_string(),
                args,
            });
        }
        let started = Instant::now();
        let answers = self.host.call(batch).map_err(Flow::Fatal)?;
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut answers = answers.into_iter();
        for promise in promises {
            let Some(index) = promise.call.get() else {
                continue;
            };
            if !matches!(&*promise.state.borrow(), PromiseState::Running) {
                continue;
            }
            let answer = answers
                .next()
                .unwrap_or_else(|| Err("the host gave no answer for this call".to_owned()));
            let settled = match answer {
                Ok(json) => match crate::value::json_parse(&json) {
                    Ok(value) => {
                        self.alloc(json.len() * 2)?;
                        self.calls[index].status = CallStatus::Ok;
                        Settled::Fulfilled(value)
                    }
                    Err(error) => {
                        self.calls[index].status = CallStatus::Error;
                        Settled::Rejected(self.error_value(
                            "Error",
                            &format!("the tool's result is not JSON: {error}"),
                        ))
                    }
                },
                Err(message) => {
                    self.calls[index].status = CallStatus::Error;
                    let error = self.error_value("Error", &message);
                    if let Value::Object(object) = &error {
                        object.props.borrow_mut().insert(
                            Rc::from("tool"),
                            Value::str(self.calls[index].name.as_str()),
                        );
                    }
                    Settled::Rejected(error)
                }
            };
            self.calls[index].duration_ms = duration_ms;
            Self::settle(promise, settled);
        }
        Ok(())
    }
}

/// What a call's callee is, for "x is not a function".
fn describe_callee(expr: &Expr) -> String {
    match expr {
        Expr::Ident(name) => name.to_string(),
        Expr::This => "this".to_owned(),
        Expr::Member { object, key, .. } => match key {
            MemberKey::Name(name) => format!("{}.{name}", describe_callee(object)),
            MemberKey::Computed(_) => format!("{}[...]", describe_callee(object)),
        },
        Expr::Call { callee, .. } => format!("{}(...)", describe_callee(callee)),
        Expr::Chain(inner) => describe_callee(inner),
        _ => "expression".to_owned(),
    }
}

/// Declare, as `undefined`, every `var` of a function body (or the
/// script) in its scope, where it is not declared yet: `var` is hoisted.
fn hoist_vars(body: &[Stmt], scope: &Env) {
    let mut names = Vec::new();
    for stmt in body {
        var_names(stmt, &mut names);
    }
    for name in names {
        if !scope.has_own(&name) {
            scope.declare(name, Value::Undefined, true);
        }
    }
}

/// The `var` names a statement declares, not counting nested functions.
fn var_names(stmt: &Stmt, out: &mut Vec<Rc<str>>) {
    match &stmt.kind {
        StmtKind::Var(VarKind::Var, declarators) => {
            for declarator in declarators {
                pattern_names(&declarator.target, out);
            }
        }
        StmtKind::If(_, then, otherwise) => {
            var_names(then, out);
            if let Some(otherwise) = otherwise {
                var_names(otherwise, out);
            }
        }
        StmtKind::For { init, body, .. } => {
            if let Some(ForInit::Var(VarKind::Var, declarators)) = init {
                for declarator in declarators {
                    pattern_names(&declarator.target, out);
                }
            }
            var_names(body, out);
        }
        StmtKind::ForEach { head, body, .. } => {
            if let ForHead::Var(VarKind::Var, target) = head {
                pattern_names(target, out);
            }
            var_names(body, out);
        }
        StmtKind::While(_, body) | StmtKind::DoWhile(body, _) | StmtKind::Labeled(_, body) => {
            var_names(body, out);
        }
        StmtKind::Block(body) => {
            for stmt in body {
                var_names(stmt, out);
            }
        }
        StmtKind::Try {
            block,
            handler,
            finalizer,
            ..
        } => {
            for part in [Some(block), handler.as_ref(), finalizer.as_ref()]
                .into_iter()
                .flatten()
            {
                for stmt in part {
                    var_names(stmt, out);
                }
            }
        }
        StmtKind::Switch(_, cases) => {
            for case in cases {
                for stmt in &case.body {
                    var_names(stmt, out);
                }
            }
        }
        _ => {}
    }
}

fn pattern_names(pattern: &Pattern, out: &mut Vec<Rc<str>>) {
    match pattern {
        Pattern::Ident(name) => out.push(name.clone()),
        Pattern::Object { props, rest } => {
            for prop in props {
                pattern_names(&prop.target, out);
            }
            if let Some(rest) = rest {
                out.push(rest.clone());
            }
        }
        Pattern::Array { items, rest } => {
            for item in items.iter().flatten() {
                pattern_names(&item.target, out);
            }
            if let Some(rest) = rest {
                pattern_names(rest, out);
            }
        }
        Pattern::Member(_) => {}
    }
}

pub fn quote(text: &str) -> String {
    let mut out = String::new();
    crate::value::json_quote(text, &mut out);
    out
}

/// ECMAScript `ToInt32`.
pub fn to_int32(value: f64) -> i32 {
    if !value.is_finite() {
        return 0;
    }
    let wrapped = value.trunc().rem_euclid(4_294_967_296.0);
    let wrapped = if wrapped >= 2_147_483_648.0 {
        wrapped - 4_294_967_296.0
    } else {
        wrapped
    };
    wrapped as i32
}

pub fn to_uint32(value: f64) -> u32 {
    if !value.is_finite() {
        return 0;
    }
    value.trunc().rem_euclid(4_294_967_296.0) as u32
}

fn js_remainder(left: f64, right: f64) -> f64 {
    if right.is_infinite() && left.is_finite() {
        return left;
    }
    left % right
}

fn js_pow(base: f64, exponent: f64) -> f64 {
    if exponent.is_nan() || (base.abs() == 1.0 && exponent.is_infinite()) {
        return f64::NAN;
    }
    base.powf(exponent)
}
