#include <setjmp.h>
#include <stddef.h>

typedef void *(*caribou_ash_trap_setup_fn)(void *storage, size_t size);
typedef void (*caribou_ash_trap_remove_fn)(void *storage);
typedef void (*caribou_ash_trap_callback_fn)(void *context);

/*
 * Room for HashLink's trap context, which is its jmp_buf and a few words.
 * The runtime says how much it wants and refuses less; this is more than
 * any target's.
 */
#define CARIBOU_ASH_TRAP_STORAGE 512

/*
 * Run `callback` under a HashLink trap whose setjmp frame is this one.
 *
 * HashLink raises by longjmp, so the frame that set the jump buffer must be
 * alive until the callee has returned. A Rust helper cannot own it: it would
 * return before the callee ran, and the jump would land in a dead frame.
 * `setup` arms the trap in this frame's own storage and hands back its
 * buffer; `remove` disarms the trap in that storage after a normal return.
 * A throw pops the trap itself before it jumps, so nothing is removed on
 * that path. Returns 0 on a normal return, 1 on a throw, 2 when the
 * runtime wants more storage.
 */
int caribou_ash_run_with_hl_trap(
    caribou_ash_trap_setup_fn setup,
    caribou_ash_trap_remove_fn remove,
    caribou_ash_trap_callback_fn callback,
    void *context) {
    _Alignas(16) unsigned char storage[CARIBOU_ASH_TRAP_STORAGE];
    jmp_buf *buffer = (jmp_buf *)setup(storage, sizeof storage);
    if (buffer == NULL) {
        return 2;
    }
#if defined(_WIN32)
    if (setjmp(*buffer) != 0) {
#else
    if (_setjmp(*buffer) != 0) {
#endif
        return 1;
    }
    callback(context);
    remove(storage);
    return 0;
}
