# GW Borrow-Checker Guidance

Audience: anyone writing GW code that needs to pass the borrow checker on the first try. Optimized for code-generating language models and humans writing in unfamiliar styles.

Status: companion to the language spec, not normative. The spec is the rules; this is the patterns.

## 1. Mental Model

Before writing any GW function, decide three things:

1. **Who owns each piece of data?** Owners are values held by `let`; references are temporary views (`&T`, `&mut T`).
2. **How long do references need to live?** GW's borrow checker is *function-local*. A reference cannot outlive the value it points into. Plan reference lifetimes scope-by-scope.
3. **Is the data graph-shaped?** If yes, you almost certainly want arena + indices, not borrows.

If you find yourself fighting the checker, the pattern is usually wrong, not the rules.

## 2. The Fundamental Rule

At any point, for any place `p`:

- Zero or more shared borrows `&p` may coexist, OR
- Exactly one mutable borrow `&mut p` may exist.

That's it. The fight is always: "I have a borrow, then I try to take an incompatible one." The win is always: end the first borrow before starting the second.

## 3. Patterns

### Pattern 1 — Arena + indices for graphs

**Use when:** any data structure with cycles, back-references, or non-tree topology. Linked lists, doubly-linked lists, graphs, ASTs with parent pointers, ECS-style entity systems.

**Don't write this:**

```gw
class Node {
    value: i32,
    next: ?&Node,         // borrow checker rejects: &Node needs an outlives constraint
    prev: ?&Node,         // same
}
```

**Write this:**

```gw
class Node {
    value: i32,
    next: ?u32,           // index into the arena
    prev: ?u32,
}

class List {
    nodes: [dyn]Node,
    head: ?u32,
}

impl List {
    fn push(self: &mut Self, v: i32) -> u0 {
        let idx = self.nodes.len() as u32;
        self.nodes.push(Node{ .value = v, .next = self.head, .prev = nil });
        if let h = self.head { self.nodes[h].prev = idx; }
        self.head = idx;
    }
}
```

The indices are plain `u32`s; the borrow checker doesn't see them as borrows. The arena owns every node. To "follow" a pointer, write `&self.nodes[idx]`. The borrow lives only for the duration of the access.

**Why this works:** the borrow checker never sees aliasing because there are no references in the data structure at rest. Aliasing only appears during access, scoped to a single statement.

### Pattern 2 — Slot maps for stable handles

**Use when:** you need stable handles into a collection that supports both insertion and deletion, and you want to detect use-after-free in safe code.

```gw
class Handle { idx: u32, gen: u32 }

class SlotMap[T: any] {
    slots: [dyn]Slot[T],
    free: ?u32,
}

class Slot[T] {
    gen: u32,
    payload: ?T,
    next_free: ?u32,
}

impl[T: any] SlotMap[T] {
    fn insert(self: &mut Self, v: T) -> Handle { ... }
    fn get(self: &Self, h: Handle) -> ?&T {
        let s = &self.slots[h.idx];
        if s.gen != h.gen then return nil;
        return s.payload.as_ref();
    }
    fn remove(self: &mut Self, h: Handle) -> ?T { ... }
}
```

Handles survive deletion of other slots; they detect use-after-free via the generation counter. This is the Bevy/Specs/Flecs pattern.

### Pattern 3 — Pass by parameter, not by field

**Use when:** a function needs to access two pieces of a struct, one mutably and one shared.

**Don't write this:**

```gw
class Engine {
    renderer: Renderer,
    physics: Physics,
}

impl Engine {
    fn step(self: &mut Self) -> u0 {
        self.physics.tick(&self.renderer);   // OK: physics is &mut, renderer is &
        self.renderer.draw(&self.physics);   // ERROR: now physics needs to be &, but we held &mut
    }
}
```

**Write this:**

```gw
impl Engine {
    fn step(self: &mut Self) -> u0 {
        tick(&mut self.physics, &self.renderer);
        draw(&mut self.renderer, &self.physics);
    }
}

fn tick(p: &mut Physics, r: &Renderer) -> u0 { ... }
fn draw(r: &mut Renderer, p: &Physics) -> u0 { ... }
```

GW's borrow checker can see "disjoint fields of the same struct" through direct field access in the caller. Pulling logic out into free functions that take field references makes the disjoint-borrow pattern explicit.

### Pattern 4 — Scope reduction

**Use when:** "I borrowed this twenty lines ago and now I want to mutate."

**Don't write this:**

```gw
let first = &v[0];           // immutable borrow starts here
do_lots_of_things();         // ... and stays alive across the call
v.push(42);                  // ERROR: shared borrow still live
print("%\n", first);
```

**Write this:**

```gw
{
    let first = &v[0];
    print("first = %\n", first);
}                            // immutable borrow ends here
v.push(42);                  // OK
```

Or, equivalently, copy the value out:

```gw
let first = v[0];            // copy (if `Copy`); now no borrow
v.push(42);                  // OK
```

The checker is lexical: a borrow ends at the end of its lexical scope. Smaller scopes mean shorter borrows.

### Pattern 5 — Collect-then-mutate

**Use when:** you want to mutate elements of a collection while iterating, or iterate twice over the same data.

**Don't write this:**

```gw
for x in &items {
    if x.bad { items.remove(x); }    // ERROR: items borrowed by iterator
}
```

**Write this:**

```gw
let to_remove: [dyn]u32 = .{};
for i, x in items.indexed() {
    if x.bad then to_remove.push(i);
}
for i in to_remove.iter_reverse() {
    items.remove_at(i);
}
```

Or, when applicable, use a filter-in-place idiom:

```gw
items.retain(|x| !x.bad);
```

The stdlib's `retain` is implemented in `manual` mode internally and exposes a safe interface that does the right thing.

### Pattern 6 — `lock { }` for shared mutable state

**Use when:** multiple tasks need to write to the same data.

```gw
let state = Arc.new(Mutex.new(GameState.new()));

task {
    lock state {
        state.update_physics(dt);
    }
};
task {
    lock state {
        state.render();
    }
};
```

`lock { }` blocks acquire the mutex for the duration of the block. Inside, the held value is mutably accessible. The block boundary is the release point.

For lock-free patterns, use `atomic[Order] T` directly; these are not borrow-checked.

### Pattern 7 — `manual` as escape hatch

**Use when:** the algorithm is correct and you can argue it locally, but the checker can't see it. Doubly-linked lists, intrusive collections, custom allocators.

```gw
pub manual fn unlink(nodes: &mut [dyn]Node, idx: u32) -> u0 {
    let n = &nodes[idx];
    if let prev_idx = n.prev {
        nodes[prev_idx].next = n.next;
    }
    if let next_idx = n.next {
        nodes[next_idx].prev = n.prev;
    }
    nodes[idx].next = nil;
    nodes[idx].prev = nil;
}
```

`manual` retains null-checking and bounds-checking; it only suppresses aliasing analysis. Reserve it for code that has been proven correct by other means (typically, careful invariant maintenance plus testing).

### Pattern 8 — `unsafe` as last resort

**Use when:** you need raw pointers, FFI, inline assembly, manual layout, or a primitive the checker fundamentally cannot model.

```gw
unsafe {
    let raw: [*]u8 = std.mem.heap.alloc_raw(1024, 16);
    defer std.mem.heap.free_raw(raw, 1024, 16);
    // raw byte twiddling
}
```

`unsafe` blocks should be small, well-commented, and wrapped in a safe interface. If you find yourself writing pages of `unsafe`, you're probably building a primitive that the stdlib should grow.

## 4. Anti-Patterns

These look reasonable but cost you the next two hours.

### Anti-Pattern 1 — Self-referential classes

```gw
class Parser {
    source: []u8,
    cursor: &u8,              // points into self.source
}
```

Don't. There is no idiomatic way to express this safely. Replace `cursor: &u8` with `cursor: usize` (a byte offset into `source`).

### Anti-Pattern 2 — Long-lived `&mut` in a class field

```gw
class Renderer {
    target: &mut Surface,     // lives as long as Renderer
}
```

Possible, but every method of `Renderer` now holds the surface mutably and the lifetime parameter propagates through every call site. Prefer passing `&mut Surface` to each method that needs it. Reserve class-held `&mut` for cases where the surface truly cannot be obtained any other way (e.g., a borrowed scratch buffer threaded through a deep call stack).

### Anti-Pattern 3 — Mutex on a single primitive value

```gw
let counter = Arc.new(Mutex.new(0i64));
// ... use `lock counter { ... }` everywhere
```

For single primitive values, prefer `atomic[Order] i64`. Mutexes carry park/unpark machinery; an atomic is a single hardware instruction.

### Anti-Pattern 4 — Holding `&` across `await`

```gw
async fn process(s: &State) -> !u0 {
    let item = &s.queue[0];
    try fetch_remote().await;     // s and item must live across await
    process_item(item);
}
```

References held across `await` points compile in some cases but rapidly force lifetime annotations to propagate up the async chain. Copy out, await, then re-borrow:

```gw
async fn process(s: &mut State) -> !u0 {
    let item = s.queue[0].clone();   // or pop, depending on semantics
    try fetch_remote().await;
    process_item(&item);
}
```

### Anti-Pattern 5 — `clone()` to silence the borrow checker

If your only fix is `.clone()` on every line, you're probably building the wrong data structure. Step back. Is this a graph that should be arena + indices? Is this a hot loop that should accumulate into a result rather than borrow recursively?

`clone()` is correct when the data is genuinely cheap and the alternative is convoluted. It's wrong when applied indiscriminately as a workaround.

## 5. Quick Reference

| Data shape                                    | Pattern                                                      |
|-----------------------------------------------|--------------------------------------------------------------|
| Tree of unique-ownership children             | Direct fields, `Box[Child]` if recursive                     |
| Tree where children are sometimes shared      | `Rc[Child]` (post-1.0) or arena indices                      |
| Graph with cycles                             | Arena + indices (Pattern 1)                                  |
| Collection with stable handles                | SlotMap (Pattern 2)                                          |
| Two class fields mutated together             | Free functions taking field refs (Pattern 3)                 |
| Shared mutable state across tasks             | `Arc[Mutex[T]]` + `lock { }`                                 |
| Single counter shared across tasks            | `atomic[Order] T`                                            |
| ECS / large heterogeneous world               | SlotMap per component type                                   |
| Doubly-linked / intrusive                     | `manual` block (Pattern 7)                                   |
| FFI / raw memory                              | `unsafe` block (Pattern 8)                                   |

## 6. Diagnosing a Borrow Error

When the checker rejects code, walk this list in order:

1. **Read the primary span and the secondary spans.** The primary is where the conflict surfaces; the secondaries are usually where the conflicting borrow started.

2. **Identify the two borrows in conflict.** Usually one is "long-lived" (held in a let-binding) and one is "newly attempted." Either shorten the first or defer the second.

3. **Is the data graph-shaped?** If yes, jump to Pattern 1. The borrow checker is not the obstacle; the data structure is.

4. **Can the long-lived borrow be made shorter?** Pattern 4 (scope reduction). Wrap it in a block or copy it out.

5. **Can the function be split?** Pattern 3 (parameter passing). One function holding two field borrows is often two functions, each holding one.

6. **Is the data hot and shared across tasks?** Pattern 6 (lock blocks) or atomics.

7. **Is this a stdlib-implementable primitive?** If yes, `manual` (Pattern 7). Wrap in a safe interface.

8. **Is this raw memory, FFI, or assembly?** `unsafe` (Pattern 8). Keep it small and commented.

## 7. When in Doubt

Default to:
- Plain `let` (ownership) over `&`/`&mut`.
- Owned data in class fields, not borrowed.
- `usize`/`u32` indices over `&T` pointers in long-lived data.
- Free functions over methods, when disjoint field access is needed.
- `Arc[Mutex[T]]` + `lock { }` over hand-rolled synchronization.
- Standard collections (`HashMap`, `[dyn]T`, `SlotMap`) over hand-rolled equivalents.

The borrow checker rewards programs that look like dataflow: data flows in, gets transformed, flows out. It punishes programs that look like state machines holding back-references. When you find yourself fighting the checker, ask whether the program could be expressed as dataflow.
