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
`attach_host_state`, or to the main context under `TaskId::NONE`; each
adapter's state is its own type, and a task carries one of each. What
belongs to a stack rather than a task, Ash's trap chain and pending
exception above all, since a trap is a frame, is attached to the stack
instead (`attach_stack_host_state`, by krio's id, 0 the thread's own),
and swapped on every switch of stacks on the thread (`switch_stack`): the
world's own, around a fiber task's turn, and a hosted runtime's own
`Fiber.call` and `yield`, told through its adapter. An adapter attaches
to a stack it first sees from the stack hook (`add_stack_hook`). The
bridge's count of guards on the stack goes with it. Zyntax will keep its
effect handler stack the same way. Around each resume the scheduler:

1. swaps the main context's state out and the task's in;
2. switches from the thread's stack to the task's, when it has one;
3. steps the task;
4. switches back;
5. publishes the task's suspended stack pointer to the heap;
6. runs the world's switch hook;
7. swaps the task's state out and the main context's back in.

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

## Another runtime's tasks

A runtime with a scheduler of its own puts its tasks on the world rather
than beside it. wren_lift is the first: its `Fiber.spawn`, `Fiber.sleep`,
`Lock`, `Channel` and `Thread` keep their front, and the world behind them
is the core's, through the World slots of its seam (see
[adapters.md](adapters.md#the-world)).

Such a task is stepped by the scheduler on its own stack, like a stackless
one, but its step runs on a stack the runtime made and switches itself:
`spawn_task(task, suspend)` takes the runtime's own switch as the task's
`Suspend`, so `park` and `yield_now` work from inside the step as on a
fiber, and the step returns `Pending` when they do. A park the runtime
asks for itself is `request_park` followed by its own switch. Either way
the world parks the task when the step returns, and finishes the wait's
registration when it wakes the task, so `resume_cause` is all the task
reads on its next step. `spawn_task_on_pool` places one on the least
loaded world, as `spawn_fiber_on_pool` places a fiber.

A runtime that carries a wait token as the number alone binds it to the
calling context when it uses it: `adopt(token)` gives the waiter for the
task that is about to park on it, `wake_token` wakes by token, and
`notified_before_park` finds a wake that came first.

Host state is per task and per stack whoever spawned or made them.
`add_task_hook` registers a function the world runs before a task's
first turn, and `add_stack_hook` one for a stack's first turn on a
thread: Ash attaches its exception state to every stack there, so a Haxe
call from a Wren task that parks inside a `try` keeps its traps to
itself, and so does a Haxe try open on a Wren fiber while another fiber
of the same task runs. The Wren adapter attaches to every task and the
main context the view's safe state and the run the context is in (the
fiber, its error state, the JIT's per-thread state), which wren_lift
sets aside and takes up again (`VM::set_aside`, `take_up`) so two
contexts inside Wren calls on one thread each resume into their own.

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
- the per-task and per-stack host state the scheduler swaps, attached from
  the task and stack hooks for every task and stack and from its own
  tasks' first run;
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
