//! `Array.prototype`, `Map` and `Set`.
//!
//! A callback may change the array it walks; each method walks a copy taken
//! when it started, which is what a script that filters or maps sees.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::rc::Rc;

use indexmap::IndexMap;

use crate::builtins::{arg, relative_index, require_function, to_integer};
use crate::interp::{Interp, MAX_ARRAY_LENGTH, R};
use crate::value::{Array, MapKey, Value, same_value_zero, strict_equals, to_string};

pub const ARRAY_METHODS: &[&str] = &[
    "at",
    "concat",
    "entries",
    "every",
    "fill",
    "filter",
    "find",
    "findIndex",
    "findLast",
    "findLastIndex",
    "flat",
    "flatMap",
    "forEach",
    "includes",
    "indexOf",
    "join",
    "keys",
    "lastIndexOf",
    "map",
    "pop",
    "push",
    "reduce",
    "reduceRight",
    "reverse",
    "shift",
    "slice",
    "some",
    "sort",
    "splice",
    "toReversed",
    "toSorted",
    "toSpliced",
    "unshift",
    "values",
    "with",
    "toString",
];
pub const MAP_METHODS: &[&str] = &[
    "get", "set", "has", "delete", "clear", "forEach", "keys", "values", "entries",
];
pub const SET_METHODS: &[&str] = &[
    "add", "has", "delete", "clear", "forEach", "keys", "values", "entries",
];

fn index(value: usize) -> Value {
    Value::Number(value as f64)
}

/// The order `sort()` puts two items in: `undefined` last, then the
/// comparator, else their string forms.
fn compare(interp: &mut Interp<'_>, comparator: &Value, a: &Value, b: &Value) -> R<Ordering> {
    match (a, b) {
        (Value::Undefined, Value::Undefined) => return Ok(Ordering::Equal),
        (Value::Undefined, _) => return Ok(Ordering::Greater),
        (_, Value::Undefined) => return Ok(Ordering::Less),
        _ => {}
    }
    if matches!(comparator, Value::Undefined) {
        return Ok(to_string(a).cmp(&to_string(b)));
    }
    let result = crate::value::to_number(&interp.call(
        comparator,
        Value::Undefined,
        vec![a.clone(), b.clone()],
    )?);
    Ok(if result < 0.0 {
        Ordering::Less
    } else if result > 0.0 {
        Ordering::Greater
    } else {
        Ordering::Equal
    })
}

/// A stable merge sort whose comparator may throw.
fn sort_values(interp: &mut Interp<'_>, items: Vec<Value>, comparator: &Value) -> R<Vec<Value>> {
    if items.len() <= 1 {
        return Ok(items);
    }
    let mut items = items;
    let mut width = 1;
    let len = items.len();
    let mut buffer: Vec<Value> = Vec::with_capacity(len);
    while width < len {
        buffer.clear();
        let mut start = 0;
        while start < len {
            let middle = (start + width).min(len);
            let end = (start + 2 * width).min(len);
            let (mut left, mut right) = (start, middle);
            while left < middle && right < end {
                interp.tick()?;
                if compare(interp, comparator, &items[right], &items[left])? == Ordering::Less {
                    buffer.push(items[right].clone());
                    right += 1;
                } else {
                    buffer.push(items[left].clone());
                    left += 1;
                }
            }
            buffer.extend_from_slice(&items[left..middle]);
            buffer.extend_from_slice(&items[right..end]);
            start = end;
        }
        std::mem::swap(&mut items, &mut buffer);
        width *= 2;
    }
    Ok(items)
}

fn flatten(interp: &mut Interp<'_>, items: &[Value], depth: f64, out: &mut Vec<Value>) -> R<()> {
    for item in items {
        match item {
            Value::Array(inner) if depth >= 1.0 => {
                let inner = inner.items.borrow().clone();
                flatten(interp, &inner, depth - 1.0, out)?;
            }
            other => out.push(other.clone()),
        }
    }
    interp.alloc(items.len() * 16)
}

fn writable<'a>(interp: &Interp<'_>, array: &'a Rc<Array>, method: &str) -> R<&'a Rc<Array>> {
    if array.frozen.get() {
        return Err(interp.type_error(format!("Cannot {method}: the array is frozen (read only)")));
    }
    Ok(array)
}

fn grow(interp: &mut Interp<'_>, current: usize, added: usize) -> R<()> {
    if current.saturating_add(added) > MAX_ARRAY_LENGTH {
        return Err(interp.throw("RangeError", "Invalid array length"));
    }
    interp.alloc(added * 16)
}

#[allow(clippy::too_many_lines, reason = "one arm per Array.prototype method")]
pub fn array_method(
    interp: &mut Interp<'_>,
    method: &str,
    this: &Value,
    args: &[Value],
) -> R<Value> {
    let Value::Array(array) = this else {
        return Err(interp.type_error(format!(
            "Array.prototype.{method} called on a value that is not an array"
        )));
    };
    // Methods that call no callback work on the array itself; the others
    // walk a copy taken when they start. `push` in a loop stays linear.
    match method {
        "push" | "unshift" => {
            let array = writable(interp, array, method)?;
            let len = array.items.borrow().len();
            grow(interp, len, args.len())?;
            let mut items = array.items.borrow_mut();
            if method == "push" {
                items.extend(args.iter().cloned());
            } else {
                items.splice(0..0, args.iter().cloned());
            }
            return Ok(index(items.len()));
        }
        "pop" => {
            return Ok(writable(interp, array, method)?
                .items
                .borrow_mut()
                .pop()
                .unwrap_or(Value::Undefined));
        }
        "shift" => {
            let array = writable(interp, array, method)?;
            let mut items = array.items.borrow_mut();
            return Ok(if items.is_empty() {
                Value::Undefined
            } else {
                items.remove(0)
            });
        }
        "at" => {
            let items = array.items.borrow();
            let position = to_integer(&arg(args, 0));
            let position = if position < 0.0 {
                items.len() as f64 + position
            } else {
                position
            };
            return Ok(if position < 0.0 || position >= items.len() as f64 {
                Value::Undefined
            } else {
                items[position as usize].clone()
            });
        }
        "includes" | "indexOf" => {
            let wanted = arg(args, 0);
            let items = array.items.borrow();
            let start = relative_index(&arg(args, 1), items.len(), 0);
            let found = items[start..].iter().position(|item| {
                if method == "includes" {
                    same_value_zero(item, &wanted)
                } else {
                    strict_equals(item, &wanted)
                }
            });
            return Ok(if method == "includes" {
                Value::Bool(found.is_some())
            } else {
                Value::Number(found.map_or(-1.0, |position| (position + start) as f64))
            });
        }
        _ => {}
    }
    let items = array.items.borrow().clone();
    let len = items.len();
    let callback = arg(args, 0);
    let each = |interp: &mut Interp<'_>, item: &Value, position: usize| {
        interp.call(
            &callback,
            arg(args, 1),
            vec![item.clone(), index(position), this.clone()],
        )
    };
    Ok(match method {
        "concat" => {
            let mut joined = items;
            for value in args {
                match value {
                    Value::Array(other) => joined.extend(other.items.borrow().iter().cloned()),
                    other => joined.push(other.clone()),
                }
            }
            grow(interp, 0, joined.len())?;
            Value::array(joined)
        }
        "entries" => Value::array(
            items
                .into_iter()
                .enumerate()
                .map(|(position, item)| Value::array(vec![index(position), item]))
                .collect(),
        ),
        "keys" => Value::array((0..len).map(index).collect()),
        "values" => Value::array(items),
        "every" | "some" | "find" | "findIndex" | "filter" | "forEach" | "map" | "findLast"
        | "findLastIndex" => {
            require_function(interp, &callback, &format!("Array.prototype.{method}"))?;
            let order: Vec<usize> = if method.starts_with("findLast") {
                (0..len).rev().collect()
            } else {
                (0..len).collect()
            };
            let mut kept = Vec::new();
            for position in order {
                interp.tick()?;
                let item = &items[position];
                let result = each(interp, item, position)?;
                match method {
                    "every" if !result.truthy() => return Ok(Value::Bool(false)),
                    "some" if result.truthy() => return Ok(Value::Bool(true)),
                    "find" | "findLast" if result.truthy() => return Ok(item.clone()),
                    "findIndex" | "findLastIndex" if result.truthy() => return Ok(index(position)),
                    "filter" if result.truthy() => kept.push(item.clone()),
                    "map" => kept.push(result),
                    _ => {}
                }
            }
            match method {
                "every" => Value::Bool(true),
                "some" => Value::Bool(false),
                "findIndex" | "findLastIndex" => Value::Number(-1.0),
                "filter" | "map" => {
                    interp.alloc(kept.len() * 16)?;
                    Value::array(kept)
                }
                _ => Value::Undefined,
            }
        }
        "fill" => {
            let array = writable(interp, array, "fill")?;
            let start = relative_index(&arg(args, 1), len, 0);
            let end = relative_index(&arg(args, 2), len, len);
            let mut items = array.items.borrow_mut();
            for item in items.iter_mut().take(end).skip(start) {
                *item = arg(args, 0);
            }
            this.clone()
        }
        "flat" => {
            let depth = match arg(args, 0) {
                Value::Undefined => 1.0,
                value => to_integer(&value),
            };
            let mut out = Vec::new();
            flatten(interp, &items, depth, &mut out)?;
            Value::array(out)
        }
        "flatMap" => {
            require_function(interp, &callback, "Array.prototype.flatMap")?;
            let mut out = Vec::new();
            for (position, item) in items.iter().enumerate() {
                interp.tick()?;
                match each(interp, item, position)? {
                    Value::Array(inner) => out.extend(inner.items.borrow().iter().cloned()),
                    other => out.push(other),
                }
            }
            interp.alloc(out.len() * 16)?;
            Value::array(out)
        }
        "lastIndexOf" => {
            let wanted = arg(args, 0);
            Value::Number(
                items
                    .iter()
                    .rposition(|item| strict_equals(item, &wanted))
                    .map_or(-1.0, |position| position as f64),
            )
        }
        "join" | "toString" => {
            let separator = match arg(args, 0) {
                Value::Undefined => ",".to_owned(),
                value if method == "join" => to_string(&value),
                _ => ",".to_owned(),
            };
            let mut text = String::new();
            for (position, item) in items.iter().enumerate() {
                if position > 0 {
                    text.push_str(&separator);
                }
                if !item.is_nullish() {
                    text.push_str(&to_string(item));
                }
            }
            interp.alloc(text.len())?;
            Value::str(text)
        }
        "reduce" | "reduceRight" => {
            require_function(interp, &callback, &format!("Array.prototype.{method}"))?;
            let order: Vec<usize> = if method == "reduce" {
                (0..len).collect()
            } else {
                (0..len).rev().collect()
            };
            let mut order = order.into_iter();
            let mut accumulator = if args.len() >= 2 {
                args[1].clone()
            } else {
                match order.next() {
                    Some(first) => items[first].clone(),
                    None => {
                        return Err(
                            interp.type_error("Reduce of empty array with no initial value")
                        );
                    }
                }
            };
            for position in order {
                interp.tick()?;
                accumulator = interp.call(
                    &callback,
                    Value::Undefined,
                    vec![
                        accumulator,
                        items[position].clone(),
                        index(position),
                        this.clone(),
                    ],
                )?;
            }
            accumulator
        }
        "reverse" => {
            let array = writable(interp, array, "reverse")?;
            array.items.borrow_mut().reverse();
            this.clone()
        }
        "toReversed" => {
            let mut items = items;
            items.reverse();
            Value::array(items)
        }
        "slice" => {
            let start = relative_index(&arg(args, 0), len, 0);
            let end = relative_index(&arg(args, 1), len, len);
            let sliced = if start < end {
                items[start..end].to_vec()
            } else {
                Vec::new()
            };
            interp.alloc(sliced.len() * 16)?;
            Value::array(sliced)
        }
        "sort" | "toSorted" => {
            let comparator = arg(args, 0);
            if !matches!(comparator, Value::Undefined | Value::Function(_)) {
                return Err(interp
                    .type_error("The comparison function must be either a function or undefined"));
            }
            let sorted = sort_values(interp, items, &comparator)?;
            if method == "toSorted" {
                Value::array(sorted)
            } else {
                *writable(interp, array, "sort")?.items.borrow_mut() = sorted;
                this.clone()
            }
        }
        "splice" | "toSpliced" => {
            let start = relative_index(&arg(args, 0), len, 0);
            let count = match args.len() {
                0 => 0,
                1 => len - start,
                _ => (to_integer(&args[1]).max(0.0) as usize).min(len - start),
            };
            let inserted: Vec<Value> = args.iter().skip(2).cloned().collect();
            grow(interp, len, inserted.len())?;
            if method == "toSpliced" {
                let mut copy = items;
                copy.splice(start..start + count, inserted);
                Value::array(copy)
            } else {
                let array = writable(interp, array, "splice")?;
                let removed: Vec<Value> = array
                    .items
                    .borrow_mut()
                    .splice(start..start + count, inserted)
                    .collect();
                Value::array(removed)
            }
        }
        "with" => {
            let position = to_integer(&arg(args, 0));
            let position = if position < 0.0 {
                len as f64 + position
            } else {
                position
            };
            if position < 0.0 || position >= len as f64 {
                return Err(interp.throw("RangeError", "Invalid index"));
            }
            let mut copy = items;
            copy[position as usize] = arg(args, 1);
            Value::array(copy)
        }
        _ => Value::Undefined,
    })
}

type MapCell = Rc<RefCell<IndexMap<MapKey, (Value, Value)>>>;
type SetCell = Rc<RefCell<IndexMap<MapKey, Value>>>;

pub fn new_map(interp: &mut Interp<'_>, source: &Value) -> R<Value> {
    let mut entries = IndexMap::new();
    if !source.is_nullish() {
        for entry in interp.iterate(source)? {
            let key = crate::builtins::get_property(interp, &entry, &Value::Number(0.0))?;
            let value = crate::builtins::get_property(interp, &entry, &Value::Number(1.0))?;
            entries.insert(key.map_key(), (key, value));
        }
    }
    interp.alloc(entries.len() * 48)?;
    Ok(Value::map(entries))
}

pub fn new_set(interp: &mut Interp<'_>, source: &Value) -> R<Value> {
    let mut entries = IndexMap::new();
    if !source.is_nullish() {
        for item in interp.iterate(source)? {
            entries.insert(item.map_key(), item);
        }
    }
    interp.alloc(entries.len() * 32)?;
    Ok(Value::set(entries))
}

pub fn map_method(interp: &mut Interp<'_>, method: &str, this: &Value, args: &[Value]) -> R<Value> {
    let Value::Map(map) = this else {
        return Err(interp.type_error(format!(
            "Map.prototype.{method} called on a value that is not a Map"
        )));
    };
    let map: &MapCell = map;
    let key = arg(args, 0);
    Ok(match method {
        "get" => map
            .borrow()
            .get(&key.map_key())
            .map_or(Value::Undefined, |(_, value)| value.clone()),
        "set" => {
            interp.alloc(48)?;
            map.borrow_mut().insert(key.map_key(), (key, arg(args, 1)));
            this.clone()
        }
        "has" => Value::Bool(map.borrow().contains_key(&key.map_key())),
        "delete" => Value::Bool(map.borrow_mut().shift_remove(&key.map_key()).is_some()),
        "clear" => {
            map.borrow_mut().clear();
            Value::Undefined
        }
        "keys" => Value::array(map.borrow().values().map(|(key, _)| key.clone()).collect()),
        "values" => Value::array(
            map.borrow()
                .values()
                .map(|(_, value)| value.clone())
                .collect(),
        ),
        "entries" => Value::array(
            map.borrow()
                .values()
                .map(|(key, value)| Value::array(vec![key.clone(), value.clone()]))
                .collect(),
        ),
        "forEach" => {
            let callback = arg(args, 0);
            require_function(interp, &callback, "Map.prototype.forEach")?;
            let entries: Vec<(Value, Value)> = map.borrow().values().cloned().collect();
            for (key, value) in entries {
                interp.call(&callback, arg(args, 1), vec![value, key, this.clone()])?;
            }
            Value::Undefined
        }
        _ => Value::Undefined,
    })
}

pub fn set_method(interp: &mut Interp<'_>, method: &str, this: &Value, args: &[Value]) -> R<Value> {
    let Value::Set(set) = this else {
        return Err(interp.type_error(format!(
            "Set.prototype.{method} called on a value that is not a Set"
        )));
    };
    let set: &SetCell = set;
    let item = arg(args, 0);
    Ok(match method {
        "add" => {
            interp.alloc(32)?;
            set.borrow_mut().insert(item.map_key(), item);
            this.clone()
        }
        "has" => Value::Bool(set.borrow().contains_key(&item.map_key())),
        "delete" => Value::Bool(set.borrow_mut().shift_remove(&item.map_key()).is_some()),
        "clear" => {
            set.borrow_mut().clear();
            Value::Undefined
        }
        "keys" | "values" => Value::array(set.borrow().values().cloned().collect()),
        "entries" => Value::array(
            set.borrow()
                .values()
                .map(|value| Value::array(vec![value.clone(), value.clone()]))
                .collect(),
        ),
        "forEach" => {
            let callback = arg(args, 0);
            require_function(interp, &callback, "Set.prototype.forEach")?;
            let values: Vec<Value> = set.borrow().values().cloned().collect();
            for value in values {
                interp.call(
                    &callback,
                    arg(args, 1),
                    vec![value.clone(), value, this.clone()],
                )?;
            }
            Value::Undefined
        }
        _ => Value::Undefined,
    })
}
