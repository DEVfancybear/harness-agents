//! What one run allocated, so it can all be let go when the run ends.
//!
//! Values are reference counted. A closure keeps the scope it was made in,
//! and that scope often holds the closure (`const f = x => ...`), so the
//! scope and everything it reaches would never be freed: each run would leak
//! its tool results for the life of the process. Every scope, array, object,
//! `Map` and `Set` a run makes is noted here, weakly; when the run ends, all
//! of them are emptied while they are still alive, which breaks every cycle,
//! and the emptied values are then dropped one by one, so even a chain
//! millions deep is freed without deep recursion.
//!
//! Each run has a thread of its own, so the record is thread-local.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use indexmap::IndexMap;

use crate::interp::Scope;
use crate::value::{Array, MapKey, Object, Value};

type MapCell = RefCell<IndexMap<MapKey, (Value, Value)>>;
type SetCell = RefCell<IndexMap<MapKey, Value>>;

#[derive(Default)]
struct Heap {
    scopes: Vec<Weak<Scope>>,
    arrays: Vec<Weak<Array>>,
    objects: Vec<Weak<Object>>,
    maps: Vec<Weak<MapCell>>,
    sets: Vec<Weak<SetCell>>,
    /// Entries noted since the dead ones were last dropped from the lists.
    noted: usize,
    /// When to drop them next.
    prune_at: usize,
}

thread_local! {
    static HEAP: RefCell<Heap> = RefCell::new(Heap::default());
}

const FIRST_PRUNE: usize = 4096;

fn note(add: impl FnOnce(&mut Heap)) {
    HEAP.with(|heap| {
        let Ok(mut heap) = heap.try_borrow_mut() else {
            return;
        };
        add(&mut heap);
        heap.noted += 1;
        if heap.noted >= heap.prune_at.max(FIRST_PRUNE) {
            heap.scopes.retain(|weak| weak.strong_count() > 0);
            heap.arrays.retain(|weak| weak.strong_count() > 0);
            heap.objects.retain(|weak| weak.strong_count() > 0);
            heap.maps.retain(|weak| weak.strong_count() > 0);
            heap.sets.retain(|weak| weak.strong_count() > 0);
            let live = heap.scopes.len()
                + heap.arrays.len()
                + heap.objects.len()
                + heap.maps.len()
                + heap.sets.len();
            heap.noted = 0;
            heap.prune_at = live.max(FIRST_PRUNE);
        }
    });
}

pub fn scope(scope: &Rc<Scope>) {
    note(|heap| heap.scopes.push(Rc::downgrade(scope)));
}

pub fn array(array: &Rc<Array>) {
    note(|heap| heap.arrays.push(Rc::downgrade(array)));
}

pub fn object(object: &Rc<Object>) {
    note(|heap| heap.objects.push(Rc::downgrade(object)));
}

pub fn map(map: &Rc<MapCell>) {
    note(|heap| heap.maps.push(Rc::downgrade(map)));
}

pub fn set(set: &Rc<SetCell>) {
    note(|heap| heap.sets.push(Rc::downgrade(set)));
}

/// Let go of everything the run made: empty each value while all of them
/// are kept alive, then drop the contents, then the values.
pub fn release() {
    let heap = HEAP.with(|heap| std::mem::take(&mut *heap.borrow_mut()));
    let scopes: Vec<Rc<Scope>> = heap.scopes.iter().filter_map(Weak::upgrade).collect();
    let arrays: Vec<Rc<Array>> = heap.arrays.iter().filter_map(Weak::upgrade).collect();
    let objects: Vec<Rc<Object>> = heap.objects.iter().filter_map(Weak::upgrade).collect();
    let maps: Vec<Rc<MapCell>> = heap.maps.iter().filter_map(Weak::upgrade).collect();
    let sets: Vec<Rc<SetCell>> = heap.sets.iter().filter_map(Weak::upgrade).collect();
    let mut contents: Vec<Value> = Vec::new();
    for scope in &scopes {
        contents.extend(scope.take_values());
    }
    for array in &arrays {
        contents.extend(std::mem::take(&mut *array.items.borrow_mut()));
        contents.extend(std::mem::take(&mut *array.extra.borrow_mut()).into_values());
    }
    for object in &objects {
        contents.extend(std::mem::take(&mut *object.props.borrow_mut()).into_values());
    }
    for map in &maps {
        for (_, (key, value)) in std::mem::take(&mut *map.borrow_mut()) {
            contents.push(key);
            contents.push(value);
        }
    }
    for set in &sets {
        contents.extend(std::mem::take(&mut *set.borrow_mut()).into_values());
    }
    // Every value in `contents` is still held by the lists above, so
    // dropping it frees nothing deep; then each emptied value is freed alone.
    drop(contents);
    drop((scopes, arrays, objects, maps, sets));
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use indexmap::IndexMap;

    use crate::interp::Scope;
    use crate::value::Value;

    #[test]
    fn frees_cycles_when_the_run_ends() {
        let scope = Scope::new(None, true);
        let list = Value::array(Vec::new());
        let object = Value::object(IndexMap::new());
        // A scope that holds a list that holds itself and an object that
        // holds the list: three cycles.
        if let (Value::Array(array), Value::Object(inner)) = (&list, &object) {
            array.items.borrow_mut().push(list.clone());
            array.items.borrow_mut().push(object.clone());
            inner
                .props
                .borrow_mut()
                .insert(Rc::from("list"), list.clone());
        }
        scope.declare(Rc::from("list"), list.clone(), true);
        let watched = match &list {
            Value::Array(array) => Rc::downgrade(array),
            _ => unreachable!(),
        };
        let watched_scope = Rc::downgrade(&scope);
        drop((list, object, scope));
        assert!(watched.upgrade().is_some(), "the cycle keeps it alive");
        super::release();
        assert!(watched.upgrade().is_none());
        assert!(watched_scope.upgrade().is_none());
    }
}
