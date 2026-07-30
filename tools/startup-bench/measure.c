/* measure.c - honest cold-start + peak-RSS wrapper.
 *
 * Forks and execs a child command, then wait4()s it and reads
 * getrusage(RUSAGE_CHILDREN).ru_maxrss. ru_maxrss is the kernel's own
 * high-water-mark of the child's resident set (KiB on Linux) - the exact
 * value GNU `time -v` reports as "Maximum resident set size", with zero
 * polling race. Wall time is CLOCK_MONOTONIC across the whole child
 * lifetime (exec -> exit): for a `--ticks 1` / render-one-frame-and-quit
 * child this is process-start to first-frame-ready + teardown.
 *
 * Usage:  measure <cmd> [args...]
 * Output (one line, stdout):  ELAPSED_MS=<f>  MAXRSS_KB=<u>  EXIT=<i>
 * The child's own stdout/stderr pass through unchanged.
 *
 * Build:  cc -O2 -o measure measure.c
 */
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <time.h>
#include <sys/wait.h>
#include <sys/resource.h>

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: %s <cmd> [args...]\n", argv[0]);
        return 2;
    }
    struct timespec t0, t1;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    pid_t pid = fork();
    if (pid < 0) {
        perror("fork");
        return 3;
    }
    if (pid == 0) {
        execvp(argv[1], &argv[1]);
        perror("execvp");
        _exit(127);
    }
    int status = 0;
    struct rusage ru;
    if (wait4(pid, &status, 0, &ru) < 0) {
        perror("wait4");
        return 4;
    }
    clock_gettime(CLOCK_MONOTONIC, &t1);
    double elapsed_ms = (t1.tv_sec - t0.tv_sec) * 1000.0
                      + (t1.tv_nsec - t0.tv_nsec) / 1.0e6;
    int exit_code = WIFEXITED(status) ? WEXITSTATUS(status)
                  : (WIFSIGNALED(status) ? 128 + WTERMSIG(status) : -1);
    fprintf(stdout, "ELAPSED_MS=%.2f MAXRSS_KB=%ld EXIT=%d\n",
            elapsed_ms, ru.ru_maxrss, exit_code);
    return exit_code;
}
