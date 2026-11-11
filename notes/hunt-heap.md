Two findings.

1. **[P2] Upvalue allocation silently wraps and aliases an existing capture.** [object.rs:104](/home/folk/Programs/dellingr/src/vm/object.rs:104)

   `UpvaluePool::alloc` claims to allocate an upvalue and return its reference, but narrows `self.slots.len()` from `usize` to `u32` without checking. On a 64-bit host, allocating slot `2^32` stores the new upvalue there and returns `UpvalueRef(0)`.

   This is reachable by retaining closures with distinct captures. For example, 33,554,432 closures capturing 128 fresh locals each fill the representable range without exceeding the per-function capture limit.

   The next capture aliases slot zero. Closing that capture overwrites an older closure's variable, and subsequent reads return the wrong value. GC compounds the corruption: [sweep](/home/folk/Programs/dellingr/src/vm/object.rs:134) also narrows indices, so sweeping the unreferenced high slot adds an occupied low slot to the free list.

   Use indices wide enough for the backing vectors, or reject exhaustion before mutation and propagate an ordinary error. This finding is established by source tracing; I did not exercise the enormous allocation required.

2. **[P3] `RawField` promises integer implementations it does not provide.** [raw_field.rs:49](/home/folk/Programs/dellingr/src/vm/raw_field.rs:49)

   Its documentation says it supports "every fixed-width integer type." Implementations stop at `i64` and `u64`; `i128` and `u128` are absent. Consequently, `state.raw_field::<i128>(1, "value")` fails to compile even for a value such as `1`. The sealed trait prevents callers from supplying the missing conversion.

   Either enumerate the supported types in the documentation or fulfill the existing promise.

No additional confirmed defects in table storage, iteration, string interning, GC roots, or anchors. A model check using the existing debug CLI passed 10,000 mixed table writes, inserts, and removes. No working-tree files were created, modified, or deleted.
