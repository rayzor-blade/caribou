# Scheduler

`caribou::sched` is Ash's fiber scheduler, redesigned over krio-core's
`Task`. `sched/world.rs` is the per-thread world and its loop. `task.rs`
has the task kinds and the host state. `wait.rs` has the wait tokens and
parking. `preempt.rs` has the poll epoch and its timer. `pool.rs` is the
worker pool.

## Worlds and tasks

A world is one OS thread with one scheduler and one reactor. Every
language runs its concurrency on the world's scheduler. A Haxe
`sys.thread.Thread`, a Wren `Fiber` and a Zyntax `fiber def` are all
handles to scheduler tasks. A call from one language into another is a
synchronous call on the current task, so a call chain through three
languages suspends and resumes as one unit.

The unit of scheduling is krio-core's `Task`, not the fiber. There are
two kinds.

- A **stackful task** owns a krio fiber with its own machine stack. It can
  suspend from any call depth by switching stacks. Ash's threads, Wren's
  fibers and Zyntax's `fiber def` are stackful.
- A **stackless task** is a compiled state machine. Its `step` runs to the
  next suspension point and returns. Zyntax's `async` and resumable
  effects, and WrenLift's action-loop and AOT-transformed fibers, are
  stackless. On wasm, where the host cannot switch stacks, every task is
  stackless or is driven by the host's own suspension.

The scheduler does not tell them apart. It calls `step` and reads the
`Suspension` that comes back.

`spawn_fiber(stack_size, body)` makes a stackful task on the calling
world. It registers the stack with the heap and charges it as external
pressure until the task is dropped. `spawn(task)` takes any `Task`.
`spawn_fiber_on_pool` places a stackful task on the least-loaded worker
world at spawn time. The default stack is 256 KB.

## The scheduler loop

Each world holds a ready queue of task ids, a timer heap keyed by
deadline, and the tasks themselves. A turn resumes every task that was
ready when the turn began. Tasks parked on a token or a timer cost no
switch. The main context, which is the thread's original stack, drives
turns when it blocks or when the driver ticks the world. A task never
drives a turn; it yields.

Host state is a `HostState` object an adapter attaches to a task with
`attach_host_state`, or to the main context under `TaskId::NONE`. Ash
keeps its trap chain and pending exception there; Zyntax will keep its
effect handler stack. Around each resume the scheduler:

1. swaps the main context's state out and the task's in;
2. steps the task;
3. publishes the task's suspended stack pointer to the heap;
4. runs the world's switch hook;
5. swaps the task's state out and the main context's back in.

The switch hook (`set_switch_hook`, one per world) runs only after the
stack pointer is published, because a hook that publishes interpreter
roots may honour a pending collection. A task's record stays in the world
while it runs; only its body is taken out. So a running task can attach
state to itself.

## Parking

`park(waiter, deadline)` is the one blocking primitive. A waiter is a wait
token, and `wake(token)` marks it notified and moves the task to the ready
queue. What park does depends on who calls it:

- on a task, it records the request and yields;
- on the main context, it drives scheduler turns and the reactor until
  notified or timed out;
- on a thread the runtime did not create, it polls the token with a short
  sleep, because such a thread has no fiber to yield and may not run tasks.

Whether a thread drives or polls is decided by `has_world()`: a thread
that has spawned or ticked owns a world.

Locks, semaphores, conditions, deques and sleeps are all built on park and
wake. A task that parks with a deadline is also on the timer heap;
whichever fires first wins, and the other is cancelled. A stackless task
cannot yield from inside `park`. It calls `request_park`, returns
`Pending`, and reads `resume_cause` when it is next stepped.

## The reactor

Not built yet. Today, when no task is ready, the main context blocks in
`scheduler_idle` on the world's endpoint until a command arrives from
another world or the next timer is due.

The reactor will be the world's source of external wakeups beyond that:
socket readiness, file watches, channels from OS threads. Blocking I/O in
any language will register with it and park, and the reactor will wake
the token. The seam for it is marked in `world.rs`.

## Preemption and safepoints

Compiled loops poll one word on every back-edge: `POLL_EPOCH`, exported as
the symbol `caribou_poll_epoch`. `poll_epoch_address` hands code
generators its address. A timer thread bumps it every two milliseconds
while any task exists. The collector's stop request bumps it too, through
the heap's poll hook, which the first world installs.

A task that sees the epoch change calls `poll`: a heap safepoint, then a
yield on a task, or one turn on the main context. So no task can starve
the others, and the world can always be stopped. The interpreter and every
blocking primitive are safepoints as well. `enter_blocking` and
`leave_blocking` mark a task as outside the heap's reach for the length of
a native call.

## Multiple worlds

A process may run several worlds on several OS threads over the one heap.
A task is pinned to the world that created it; a krio fiber is `!Send`
and never migrates. Ash's worker pool for compiled thread bodies is the
first use. It chooses a world at spawn time and never moves the task
afterwards. Worlds exchange `Wake` and `Spawn` commands through per-world
endpoints.

The pool is sized by `CARIBOU_WORKERS`, else `ASH_WORKERS`, else the
machine. On wasm there is no pool and no timer thread, and `yield_now`
goes through krio's host suspender. Collections stop every world at its
safepoints. `CARIBOU_SCHED_TRACE` prints every switch and park; it is safe
to run with.

## Adapter contract

An adapter provides three things:

- a way to build a task from its own callable (a Haxe closure, a Wren
  fiber object, a Zyntax function), rooting that callable itself;
- the per-task host state the scheduler swaps;
- a switch hook, if it keeps interpreter roots to publish.

It uses `spawn`, `spawn_fiber`, `park`, `wake`, `yield_now`,
`sleep_until`, `poll`, `current_task`, and `tick(deadline)` when a driver
owns the frame loop. `has_worker_pool`, `is_pool_worker` and
`any_live_tasks` answer the placement and blocking questions Ash's
primitives ask before they spawn or wait. Ash's rule that a new thread
runs to its first blocking point before `thread_create` returns is the
adapter's to keep, with one `schedule_step` after spawning.

## Boundaries of the current implementation

- No reactor. Idle blocks on the endpoint and the timer heap only.
- The main stack's published probe sits above the callee-saved registers
  krio spills at a switch, as in Ash.
