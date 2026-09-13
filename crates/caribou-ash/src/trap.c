#include <setjmp.h>
#include <stddef.h>

typedef void *(*caribou_ash_trap_setup_fn)(void);
typedef void (*caribou_ash_trap_remove_fn)(void);
typedef void (*caribou_ash_trap_callback_fn)(void *context);

/*
 * Run `callback` under a HashLink trap whose setjmp frame is this one.
 *
 * HashLink raises by longjmp, so the frame that set the jump buffer must be
 * alive until the callee has returned. A Rust helper cannot own it: it would
 * return before the callee ran, and the jump would land in a dead frame.
 * `setup` arms the trap and hands back its buffer; `remove` disarms it after
 * a normal return. A throw pops the trap itself before it jumps, so nothing
 * is removed on that path. Returns 0 on a normal return, 1 on a throw.
 */
int caribou_ash_run_with_hl_trap(
    caribou_ash_trap_setup_fn setup,
    caribou_ash_trap_remove_fn remove,
    caribou_ash_trap_callback_fn callback,
    void *context) {
    jmp_buf *buffer = (jmp_buf *)setup();
#if defined(_WIN32)
    if (setjmp(*buffer) != 0) {
#else
    if (_setjmp(*buffer) != 0) {
#endif
        return 1;
    }
    callback(context);
    remove();
    return 0;
}
