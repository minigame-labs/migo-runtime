/*
 * Does withholding `com.apple.security.cs.allow-jit` actually deny executable
 * memory on THIS machine?
 *
 * `test-macos-archive-runs-js.sh --harden without-jit` is a negative control: it
 * withholds the entitlement and asserts that no working V8 comes out the other
 * side. A negative control is only worth the paper it is printed on if the
 * mechanism it withholds is one the machine enforces. If ad-hoc signing, this
 * OS version, or this architecture does not deny JIT memory to a hardened
 * process, then the run proves nothing about V8 -- and the shape of the failure
 * (a run that completes) looks exactly like the product defect the control was
 * written to catch. That is the trap this file exists to close: the gate first
 * establishes that its instrument bites, and only then reads V8's behaviour.
 *
 * So this asks the kernel directly, with no V8 involved, for the two ways a JIT
 * gets executable memory on macOS:
 *
 *   MAP_JIT        -- what V8 uses on arm64, paired with the per-thread W^X
 *                     toggle. Requires the entitlement under a hardened runtime.
 *   mprotect(EXEC) -- write to an ordinary RW page, then ask for it back as RX.
 *                     This is the x86_64 shape, and the hardened runtime denies
 *                     it without `allow-unsigned-executable-memory`.
 *
 * Each attempt runs in a forked child, because a denial does not always arrive
 * as an errno: the kernel may kill the process for a code-signing violation
 * instead, and "killed" is a different finding from "refused" -- one that a
 * probe running in-process could only report by dying with it.
 *
 * Exit 0 when executable memory was obtained (the entitlement is NOT being
 * enforced, or was granted), 42 when every mechanism was denied.
 */
#include <libkern/OSCacheControl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

/* `int f(void) { return 42; }`, so a page that merely accepted the bytes is
 * distinguishable from one that executed them. */
#if defined(__aarch64__)
static const uint8_t RETURN_42[] = {0x40, 0x05, 0x80, 0x52,   /* mov w0, #42 */
                                    0xc0, 0x03, 0x5f, 0xd6};  /* ret         */
#elif defined(__x86_64__)
static const uint8_t RETURN_42[] = {0xb8, 0x2a, 0x00, 0x00, 0x00, /* mov eax, 42 */
                                    0xc3};                        /* ret         */
#else
#error "this probe emits machine code and knows only arm64 and x86_64"
#endif

#define PROBE_PAGE 4096

typedef int (*probe_fn)(void);

enum verdict { DENIED = 0, GRANTED = 1, KILLED = 2, BROKEN = 3 };

static const char *label(enum verdict v) {
    switch (v) {
    case GRANTED: return "granted";
    case DENIED:  return "denied";
    case KILLED:  return "killed";
    default:      return "broken";
    }
}

static int attempt_map_jit(void) {
    void *page = mmap(NULL, PROBE_PAGE, PROT_READ | PROT_WRITE | PROT_EXEC,
                      MAP_PRIVATE | MAP_ANON | MAP_JIT, -1, 0);
    if (page == MAP_FAILED) return DENIED;
#if defined(__aarch64__)
    pthread_jit_write_protect_np(0);
#endif
    memcpy(page, RETURN_42, sizeof RETURN_42);
#if defined(__aarch64__)
    pthread_jit_write_protect_np(1);
    sys_icache_invalidate(page, sizeof RETURN_42);
#endif
    int value = ((probe_fn)page)();
    munmap(page, PROBE_PAGE);
    return value == 42 ? GRANTED : BROKEN;
}

static int attempt_mprotect_exec(void) {
    void *page = mmap(NULL, PROBE_PAGE, PROT_READ | PROT_WRITE,
                      MAP_PRIVATE | MAP_ANON, -1, 0);
    if (page == MAP_FAILED) return BROKEN;
    memcpy(page, RETURN_42, sizeof RETURN_42);
    if (mprotect(page, PROBE_PAGE, PROT_READ | PROT_EXEC) != 0) {
        munmap(page, PROBE_PAGE);
        return DENIED;
    }
    sys_icache_invalidate(page, sizeof RETURN_42);
    int value = ((probe_fn)page)();
    munmap(page, PROBE_PAGE);
    return value == 42 ? GRANTED : BROKEN;
}

/* A denial can arrive as a return value or as a signal. Both are answers; only
 * one of them can be reported by the process that asked. */
static enum verdict in_child(int (*attempt)(void)) {
    pid_t child = fork();
    if (child < 0) return BROKEN;
    if (child == 0) _exit(attempt());
    int status = 0;
    if (waitpid(child, &status, 0) != child) return BROKEN;
    if (WIFSIGNALED(status)) return KILLED;
    if (!WIFEXITED(status)) return BROKEN;
    int code = WEXITSTATUS(status);
    return code == GRANTED ? GRANTED : (code == DENIED ? DENIED : BROKEN);
}

int main(void) {
    enum verdict map_jit = in_child(attempt_map_jit);
    enum verdict mprotect_exec = in_child(attempt_mprotect_exec);
    printf("jit-entitlement-probe: map_jit=%s mprotect_exec=%s\n", label(map_jit),
           label(mprotect_exec));
    return (map_jit == GRANTED || mprotect_exec == GRANTED) ? 0 : 42;
}
