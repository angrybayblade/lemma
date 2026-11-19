// Candidate C for the trial (spec section 5): a library loaded with
// DYLD_INSERT_LIBRARIES that records the file calls it wraps. Every process
// it loads into writes $WATCH_DIR/calls.<pid>, beginning with a `watched`
// line; each child it starts is recorded before it starts, so a missing
// record shows a process that ran unwatched.
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <spawn.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/attr.h>
#include <sys/clonefile.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <unistd.h>

#define INTERPOSE(replacement, original) \
    __attribute__((used)) static struct { const void *r; const void *o; } interpose_##original \
    __attribute__((section("__DATA,__interpose"))) = {(const void *)(unsigned long)&replacement, \
                                                       (const void *)(unsigned long)&original};

int openbyid_np(fsid_t *fsid, fsobj_id_t *objid, int flags);
ssize_t fsgetpath(char *buffer, size_t size, fsid_t *fsid, uint64_t objid);

static int record = -1;

static void note(const char *format, ...) {
    if (record < 0) return;
    char line[8192];
    va_list arguments;
    va_start(arguments, format);
    int length = vsnprintf(line, sizeof line, format, arguments);
    va_end(arguments);
    if (length > 0) write(record, line, (size_t)length < sizeof line ? (size_t)length : sizeof line - 1);
}

static void open_record(void) {
    const char *directory = getenv("WATCH_DIR");
    if (!directory) return;
    char path[4096];
    snprintf(path, sizeof path, "%s/calls.%d", directory, getpid());
    record = open(path, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0644);
    char program[4096] = "";
    char cwd[4096] = "";
    getcwd(cwd, sizeof cwd);
    extern int *_NSGetArgc(void);
    extern char ***_NSGetArgv(void);
    snprintf(program, sizeof program, "%s", (*_NSGetArgv())[0]);
    note("watched %d parent %d program %s cwd %s\n", getpid(), getppid(), program, cwd);
}

__attribute__((constructor)) static void start(void) { open_record(); }

#define RESULT(value) ((value) < 0 ? errno : 0)

static int w_open(const char *path, int flags, ...) {
    va_list arguments; va_start(arguments, flags); int mode = va_arg(arguments, int); va_end(arguments);
    int fd = open(path, flags, mode);
    note("open %s flags %d errno %d\n", path, flags, RESULT(fd));
    return fd;
}
INTERPOSE(w_open, open)

static int w_openat(int at, const char *path, int flags, ...) {
    va_list arguments; va_start(arguments, flags); int mode = va_arg(arguments, int); va_end(arguments);
    int fd = openat(at, path, flags, mode);
    note("openat %d %s flags %d errno %d\n", at, path, flags, RESULT(fd));
    return fd;
}
INTERPOSE(w_openat, openat)

static int w_stat(const char *path, struct stat *info) { int r = stat(path, info); note("stat %s errno %d\n", path, RESULT(r)); return r; }
INTERPOSE(w_stat, stat)
static int w_lstat(const char *path, struct stat *info) { int r = lstat(path, info); note("lstat %s errno %d\n", path, RESULT(r)); return r; }
INTERPOSE(w_lstat, lstat)
static int w_fstatat(int at, const char *path, struct stat *info, int flags) { int r = fstatat(at, path, info, flags); note("fstatat %s errno %d\n", path, RESULT(r)); return r; }
INTERPOSE(w_fstatat, fstatat)
static int w_access(const char *path, int mode) { int r = access(path, mode); note("access %s errno %d\n", path, RESULT(r)); return r; }
INTERPOSE(w_access, access)
static int w_faccessat(int at, const char *path, int mode, int flags) { int r = faccessat(at, path, mode, flags); note("faccessat %s errno %d\n", path, RESULT(r)); return r; }
INTERPOSE(w_faccessat, faccessat)
static int w_getattrlist(const char *path, void *list, void *buffer, size_t size, unsigned int options) {
    int r = getattrlist(path, list, buffer, size, options); note("getattrlist %s errno %d\n", path, RESULT(r)); return r; }
INTERPOSE(w_getattrlist, getattrlist)
static int w_getattrlistbulk(int fd, struct attrlist *list, void *buffer, size_t size, uint64_t options) {
    int r = getattrlistbulk(fd, list, buffer, size, options); note("getattrlistbulk fd %d errno %d\n", fd, RESULT(r)); return r; }
INTERPOSE(w_getattrlistbulk, getattrlistbulk)
static ssize_t w_readlink(const char *path, char *buffer, size_t size) { ssize_t r = readlink(path, buffer, size); note("readlink %s errno %d\n", path, RESULT(r)); return r; }
INTERPOSE(w_readlink, readlink)
static DIR *w_opendir(const char *path) { DIR *d = opendir(path); note("opendir %s errno %d\n", path, d ? 0 : errno); return d; }
INTERPOSE(w_opendir, opendir)
static int w_chdir(const char *path) { int r = chdir(path); note("chdir %s errno %d\n", path, RESULT(r)); return r; }
INTERPOSE(w_chdir, chdir)
static int w_rename(const char *from, const char *to) { int r = rename(from, to); note("rename %s %s errno %d\n", from, to, RESULT(r)); return r; }
INTERPOSE(w_rename, rename)
static int w_unlink(const char *path) { int r = unlink(path); note("unlink %s errno %d\n", path, RESULT(r)); return r; }
INTERPOSE(w_unlink, unlink)
static int w_mkdir(const char *path, mode_t mode) { int r = mkdir(path, mode); note("mkdir %s errno %d\n", path, RESULT(r)); return r; }
INTERPOSE(w_mkdir, mkdir)
static int w_clonefile(const char *from, const char *to, uint32_t flags) { int r = clonefile(from, to, flags); note("clonefile %s %s errno %d\n", from, to, RESULT(r)); return r; }
INTERPOSE(w_clonefile, clonefile)
static int w_openbyid_np(fsid_t *fsid, fsobj_id_t *object, int flags) { int r = openbyid_np(fsid, object, flags); note("openbyid_np errno %d\n", RESULT(r)); return r; }
INTERPOSE(w_openbyid_np, openbyid_np)
static ssize_t w_fsgetpath(char *buffer, size_t size, fsid_t *fsid, uint64_t object) {
    ssize_t r = fsgetpath(buffer, size, fsid, object); note("fsgetpath %s errno %d\n", r >= 0 ? buffer : "-", RESULT(r)); return r; }
INTERPOSE(w_fsgetpath, fsgetpath)

static pid_t w_fork(void) {
    pid_t pid = fork();
    if (pid == 0) open_record();
    else note("fork child %d\n", pid);
    return pid;
}
INTERPOSE(w_fork, fork)
static int w_execve(const char *path, char *const argv[], char *const envp[]) {
    note("execve %s\n", path);
    return execve(path, argv, envp);
}
INTERPOSE(w_execve, execve)
static int w_posix_spawn(pid_t *pid, const char *path, const posix_spawn_file_actions_t *actions,
                         const posix_spawnattr_t *attributes, char *const argv[], char *const envp[]) {
    int r = posix_spawn(pid, path, actions, attributes, argv, envp);
    note("posix_spawn %s child %d errno %d\n", path, pid ? *pid : -1, r);
    return r;
}
INTERPOSE(w_posix_spawn, posix_spawn)
static int w_posix_spawnp(pid_t *pid, const char *file, const posix_spawn_file_actions_t *actions,
                          const posix_spawnattr_t *attributes, char *const argv[], char *const envp[]) {
    int r = posix_spawnp(pid, file, actions, attributes, argv, envp);
    note("posix_spawnp %s child %d errno %d\n", file, pid ? *pid : -1, r);
    return r;
}
INTERPOSE(w_posix_spawnp, posix_spawnp)

// libc's own entry points into libsystem_kernel (found with dyld_info -imports).
int __open_nocancel(const char *path, int flags, ...);
int open_nocancel(const char *path, int flags, ...) __asm("_open$NOCANCEL");
static int w_open_nocancel_private(const char *path, int flags, ...) {
    va_list arguments; va_start(arguments, flags); int mode = va_arg(arguments, int); va_end(arguments);
    int fd = __open_nocancel(path, flags, mode); note("__open_nocancel %s errno %d\n", path, RESULT(fd)); return fd; }
INTERPOSE(w_open_nocancel_private, __open_nocancel)
static int w_open_nocancel(const char *path, int flags, ...) {
    va_list arguments; va_start(arguments, flags); int mode = va_arg(arguments, int); va_end(arguments);
    int fd = open_nocancel(path, flags, mode); note("open$NOCANCEL %s errno %d\n", path, RESULT(fd)); return fd; }
INTERPOSE(w_open_nocancel, open_nocancel)
