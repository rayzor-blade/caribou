# Scheduler & Task Model

## Overview

`caribou::sched` is Ash's fiber scheduler, rebuilt on top of krio-core's `Task` type. It is the cooperative scheduler that every guest runtime runs its concurrency on. The module is split into these files:

* `sched/world.rs` holds the per-thread world and its scheduling loop.
* `task.rs` defines the task kinds and host state.
* `wait.rs` implements wait tokens and parking.
* `preempt.rs` implements the poll epoch and its timer.
* `pool.rs` implements the worker pool.

## Worlds & Tasks

A world is one OS thread with one scheduler and one reactor. Every language runs its concurrency on the world's scheduler. A Haxe `sys.thread.Thread`, a Wren `Fiber`, and a Zyntax `fiber def` are all handles to scheduler tasks. A call from one language into another is a synchronous call on the current task, so a call chain that crosses three languages suspends and resumes as a single unit.

The unit of scheduling is krio-core's `Task`, not the fiber. There are two kinds of task:

* **Stackful tasks** own a krio fiber with its own machine stack. They can suspend from any call depth by switching stacks. Ash threads, Wren fibers, and Zyntax `fiber def` functions are stackful.
* **Stackless tasks** are compiled state machines. Their `step` function runs to the next suspension point and returns. Zyntax `async` functions and resumable effects, and WrenLift's action-loop and AOT-transformed fibers, are stackless. On wasm, where the host cannot switch stacks, every task is either stackless or driven by the host's own suspension mechanism.

The scheduler does not distinguish between the two kinds. It calls `step` and inspects the `Suspension` value that comes back.

**Spawning:**

* `spawn_fiber(stack_size, body)` creates a stackful task on the calling world. It registers the stack with the heap and counts the stack as external memory pressure until the task is dropped. The default stack size is 256 KB.
* `spawn(task)` accepts any `Task`.
* `spawn_fiber_on_pool` places a stackful task on the least-loaded worker world at spawn time.

## The Scheduler Loop

Each world keeps a ready queue of task ids, a timer heap keyed by deadline, and the tasks themselves. One turn resumes every task that was ready when the turn started. Tasks parked on a token or a timer cost nothing during a turn. The main context, which is the thread's original stack, drives turns when it blocks or when the driver ticks the world. A task never drives a turn; it yields.

**Host State:** Host state is a `HostState` object that an adapter attaches to a task with `attach_host_state`, or to the main context under `TaskId::NONE`. Each adapter defines its own state type, and a task carries one object of each. Some state belongs to a stack rather than to a task. Ash's trap chain and pending exception are the main example, because a trap is a frame. This state is attached to the stack instead (`attach_stack_host_state`, keyed by krio's id, where 0 is the thread's own stack) and swapped on every stack switch on the thread (`switch_stack`). Stack switches include the world's own switches around a fiber task's turn, and a guest runtime's own `Fiber.call` and `yield`, which the runtime reports through its adapter. An adapter attaches state to a stack the first time it sees the stack from the stack hook (`add_stack_hook`). The bridge's count of guards on the stack is part of this state. Zyntax will keep its effect handler stack the same way.

**Resume Sequence:** Around each resume, the scheduler:

1. Swaps out the main context's state and swaps in the task's.
2. Switches from the thread's stack to the task's stack, if the task has one.
3. Steps the task.
4. Switches back.
5. Publishes the task's suspended stack pointer to the heap.
6. Runs the world's switch hook.
7. Swaps out the task's state and swaps the main context's back in.

The switch hook (`set_switch_hook`, one per world) runs only after the stack pointer has been published, because a hook that publishes interpreter roots may trigger a pending collection. A task's record stays in the world while the task runs; only its body is taken out. A running task can therefore attach state to itself.

## Parking & Wait Tokens

`park(waiter, deadline)` is the only blocking primitive. A waiter is a wait token. `wake(token)` marks the token as notified and moves the task to the ready queue. What `park` does depends on who calls it:

* **On a task**, it records the request and yields.
* **On the main context**, it drives scheduler turns and the reactor until the token is notified or the deadline passes.
* **On a thread the runtime did not create**, it polls the token with a short sleep. Such a thread has no fiber to yield from and may not run tasks.

`has_world()` decides whether a thread drives or polls: a thread that has spawned or ticked owns a world.

Locks, semaphores, conditions, deques, and sleeps are all built on `park` and `wake`. A task that parks with a deadline is also placed on the timer heap. Whichever fires first wins, and the other is cancelled. A stackless task cannot yield from inside `park`. It calls `request_park`, returns `Pending`, and reads `resume_cause` when it is next stepped.

## Shared Futures

`caribou.Future` is a core heap object over the same wait tokens. Plugins use
the one-word `caribou_abi::Future<T>` carrier and retain outstanding work as
`Rooted<Future<T>>`. Its ABI descriptor carries `T`, so typed frontends infer
the result of `await()`. `resolve(Value)`, `resolve_boxed(Box<T>)`, and
`reject(Value)` are first-completion wins and may run on a backend callback
thread. A boxed plugin result is installed directly in its core object rather
than serialized. The settled value remains in the future, where the collector
traces it, and no adapter copies it.

Every frontend can construct a pending future and sees the same `ready()`,
`await()`, `resolve(value)`, and `reject(error)` methods. Settlement is
first-completion wins and returns whether the call won. `await()` parks
the current Caribou task and either returns the declared value or raises the
rejection value. Haxe exposes these methods through `caribou.Future<T>`;
Wren installs the core-published class on first crossing. Zyntax async and a
browser Wren `Future` can adapt their syntax to this object while retaining
the same plugin ABI. A wasm stackless adapter must translate the wait into
`request_park`/`Pending` rather than call the blocking `await()` entry directly.

## Guest Runtime Tasks

A runtime with a scheduler of its own puts its tasks on the world rather than beside it. WrenLift is the first such runtime. Its `Fiber.spawn`, `Fiber.sleep`, `Lock`, `Channel`, and `Thread` keep their API, but the world behind them is the core's, reached through the World slots of its seam (see [adapters.md](adapters.md#the-world)).

**How these tasks run:**

* The scheduler steps such a task on its own stack, like a stackless task, but the step runs on a stack the runtime created and switches itself. `spawn_task(task, suspend)` takes the runtime's own switch function as the task's `Suspend`. `park` and `yield_now` therefore work from inside the step as they do on a fiber, and the step returns `Pending` when they are called.
* When the runtime requests a park itself, it calls `request_park` and then performs its own switch. Either way, the world parks the task when the step returns and completes the wait's registration when it wakes the task, so `resume_cause` is all the task needs to read on its next step.
* `spawn_task_on_pool` places a task on the least-loaded world, the same way `spawn_fiber_on_pool` places a fiber.

**Wait token adoption:** A runtime that passes a wait token around as a plain number binds it to the calling context when it uses it. `adopt(token)` returns the waiter for the task that is about to park on the token, `wake_token` wakes by token, and `notified_before_park` detects a wake that arrived before the park.

**Host state attachment:** Host state is per task and per stack, no matter who spawned the task or created the stack. `add_task_hook` registers a function the world runs before a task's first turn. `add_stack_hook` registers one for a stack's first turn on a thread. Ash uses the stack hook to attach its exception state to every stack, so a Haxe call from a Wren task that parks inside a `try` keeps its traps to itself, and so does a Haxe `try` that is open on a Wren fiber while another fiber of the same task runs. The Wren adapter attaches two things to every task and to the main context: the view's safe state, and the run the context is in (the fiber, its error state, and the JIT's per-thread state). WrenLift sets that run aside and picks it up again (`VM::set_aside`, `take_up`), so two contexts that are both inside Wren calls on one thread each resume into their own run.

## The Reactor

When no task is ready, the main context blocks in `scheduler_idle` on the world's endpoint until a command arrives or the next timer is due. The reactor is how commands arrive from outside the world:

* **Sources:** A *source* is a handler that the world runs on its main context, between turns, each time its signal is raised (`add_source`, `Signal::raise`, `remove_source`).
* **Cross-thread signaling:** A raise can come from any thread, including one the runtime never created. It reaches the world as a `Ready` command through the endpoint, which wakes the idle wait. The handler runs at the start of the next turn, when no task is in the middle of a resume. Raises that arrive while a handler is running are folded into one additional run.
* **Hot reload:** The world's source watch for hot reload is the first registered source (see [world.md](world.md#reload)).

A task waiting for external input is a separate case. It parks on a token, and whoever notices the input wakes the token, from any thread. The poller that would own socket readiness on the world's behalf does not exist yet, so a blocking socket read in any language still blocks the OS thread.

## Preemption & Safepoints

Compiled loops poll one word on every back-edge: `POLL_EPOCH`, exported as the symbol `caribou_poll_epoch`. `poll_epoch_address` gives code generators its address. A timer thread bumps it every two milliseconds while any task exists. The collector's stop request also bumps it, through the heap's poll hook, which the first world installs.

A task that sees the epoch change calls `poll`. `poll` performs a heap safepoint, then yields on a task or runs one turn on the main context. No task can starve the others, and the world can always be stopped. The interpreter and every blocking primitive are safepoints as well. `enter_blocking` and `leave_blocking` mark a task as outside the heap's reach for the duration of a native call.

## Multiple Worlds

A process can run several worlds on several OS threads over the single heap. A task is pinned to the world that created it; a krio fiber is `!Send` and never migrates. Ash's worker pool for compiled thread bodies is the first use of multiple worlds. It picks a world at spawn time and never moves the task afterward. Worlds exchange `Wake` and `Spawn` commands through per-world endpoints.

* **Pool size:** `CARIBOU_WORKERS`, else `ASH_WORKERS`, else the machine's core count.
* **Wasm:** There is no pool and no timer thread. `yield_now` goes through krio's host suspender.
* **Collections:** A collection stops every world at its safepoints.
* **Tracing:** `CARIBOU_SCHED_TRACE` prints every switch and park. It is safe to leave on.

## Adapter Contract

An adapter provides three things:

* **Task construction:** A way to build a task from its own callable (a Haxe closure, a Wren fiber object, a Zyntax function). The adapter roots the callable itself.
* **Host state:** The per-task and per-stack host state that the scheduler swaps. The adapter attaches it from the task and stack hooks, for every task and stack, and from its own tasks' first run.
* **Switch hook:** Only needed if the adapter keeps interpreter roots that it has to publish.

The adapter calls `spawn`, `spawn_fiber`, `park`, `wake`, `yield_now`, `sleep_until`, `poll`, `current_task`, and `tick(deadline)` when a driver owns the frame loop. `has_worker_pool`, `is_pool_worker`, and `any_live_tasks` answer the placement and blocking questions that Ash's primitives ask before they spawn or wait. Ash requires that a new thread runs to its first blocking point before `thread_create` returns; the adapter satisfies this with one `schedule_step` after spawning.

## Current Implementation Boundaries

* There is no socket poller. Idle blocks on the endpoint and the timer heap. A source's raise wakes it; a socket becoming readable does not.
* The main stack's published probe sits above the callee-saved registers that krio spills at a switch, as in Ash.
