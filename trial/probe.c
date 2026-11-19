// A probe with a known answer (spec section 6.1). It does a fixed list of
// file operations in a directory made for it, each twice, and prints each
// operation before doing it, so a recorder's output can be scored line by
// line. `probe child FILE` is the child mode: open FILE and exit.
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/attr.h>
#include <sys/clonefile.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;
// Private but exported by libSystem; declared here as the SDK leaves them out.
int openbyid_np(fsid_t *fsid, fsobj_id_t *objid, int flags);
ssize_t fsgetpath(char *buffer, size_t size, fsid_t *fsid, uint64_t objid);

static char self[4096];

static void say(const char *op, const char *path) { printf("%s %s\n", op, path); fflush(stdout); }

static void touch(const char *path, const char *text) {
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd >= 0) { write(fd, text, strlen(text)); close(fd); }
}

static void child(const char *how, const char *file) {
    char *argv[] = {self, "child", (char *)file, NULL};
    say(how, file);
    if (strcmp(how, "fork-execve") == 0) {
        pid_t pid = fork();
        if (pid == 0) { execve(self, argv, environ); _exit(127); }
        waitpid(pid, NULL, 0);
    } else if (strcmp(how, "posix_spawn") == 0) {
        pid_t pid;
        posix_spawn(&pid, self, NULL, NULL, argv, environ);
        waitpid(pid, NULL, 0);
    } else if (strcmp(how, "sh-c") == 0) {
        char command[8192];
        snprintf(command, sizeof command, "'%s' child '%s'", self, file);
        char *shell[] = {"/bin/sh", "-c", command, NULL};
        pid_t pid;
        posix_spawn(&pid, "/bin/sh", NULL, NULL, shell, environ);
        waitpid(pid, NULL, 0);
    } else if (strcmp(how, "env-script") == 0) {
        char *script[] = {"./opener.py", (char *)file, NULL};
        pid_t pid;
        posix_spawn(&pid, "./opener.py", NULL, NULL, script, environ);
        waitpid(pid, NULL, 0);
    }
}

static void once(void) {
    struct stat info;
    char buffer[4096];
    struct attrlist attributes = {.bitmapcount = ATTR_BIT_MAP_COUNT, .commonattr = ATTR_CMN_NAME | ATTR_CMN_OBJTYPE};

    say("open", "a.txt");             close(open("a.txt", O_RDONLY));
    say("open-missing", "missing.txt"); close(open("missing.txt", O_RDONLY));
    say("stat", "a.txt");             stat("a.txt", &info);
    say("stat-missing", "missing-stat.txt"); stat("missing-stat.txt", &info);
    say("lstat", "link.txt");         lstat("link.txt", &info);
    say("lstat-missing", "missing-lstat.txt"); lstat("missing-lstat.txt", &info);
    say("access", "a.txt");           access("a.txt", R_OK);
    say("access-missing", "missing-access.txt"); access("missing-access.txt", R_OK);
    say("getattrlist", "a.txt");      getattrlist("a.txt", &attributes, buffer, sizeof buffer, 0);
    say("getattrlist-missing", "missing-attr.txt"); getattrlist("missing-attr.txt", &attributes, buffer, sizeof buffer, 0);
    say("readlink", "link.txt");      readlink("link.txt", buffer, sizeof buffer);
    say("open-through-dir-link", "dirlink/inner.txt"); close(open("dirlink/inner.txt", O_RDONLY));

    say("readdir", "listed");
    DIR *directory = opendir("listed");
    if (directory) { while (readdir(directory)) {} closedir(directory); }
    say("getattrlistbulk", "bulk");
    int bulk = open("bulk", O_RDONLY | O_DIRECTORY);
    if (bulk >= 0) {
        struct attrlist list = {.bitmapcount = ATTR_BIT_MAP_COUNT, .commonattr = ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_NAME};
        while (getattrlistbulk(bulk, &list, buffer, sizeof buffer, 0) > 0) {}
        close(bulk);
    }

    say("chdir", "sub");              chdir("sub");
    say("open-relative", "sub/relative.txt"); close(open("relative.txt", O_RDONLY));
    chdir("..");

    child("fork-execve", "child-fork.txt");
    child("posix_spawn", "child-spawn.txt");
    child("sh-c", "child-sh.txt");
    child("env-script", "child-env.txt");

    say("write", "w.txt");            touch("w.txt", "w\n");
    say("rename", "w.txt -> w2.txt"); rename("w.txt", "w2.txt");
    say("unlink", "w2.txt");          unlink("w2.txt");
    say("mkdir", "made");             mkdir("made", 0755);
    rmdir("made");
    say("clonefile", "a.txt -> clone.txt"); clonefile("a.txt", "clone.txt", 0); unlink("clone.txt");

    // A file found by id, never by name: its id comes from listing its
    // directory, which reads names but opens nothing.
    struct statfs volume;
    statfs("byid", &volume);
    DIR *ids = opendir("byid");
    struct dirent *entry;
    while (ids && (entry = readdir(ids))) {
        if (strcmp(entry->d_name, "secret.txt") != 0) continue;
        say("openbyid_np", "byid/secret.txt");
        int fd = openbyid_np(&volume.f_fsid, (fsobj_id_t *)&entry->d_ino, O_RDONLY);
        if (fd >= 0) close(fd);
        say("fsgetpath", "byid/secret.txt");
        fsgetpath(buffer, sizeof buffer, &volume.f_fsid, entry->d_ino);
    }
    if (ids) closedir(ids);
}

int main(int argc, char **argv) {
    realpath(argv[0], self);
    if (argc == 3 && strcmp(argv[1], "child") == 0) {
        close(open(argv[2], O_RDONLY));
        return 0;
    }
    if (argc != 2) { fprintf(stderr, "usage: probe DIRECTORY\n"); return 2; }
    mkdir(argv[1], 0755);
    if (chdir(argv[1]) != 0) return 1;
    touch("a.txt", "a\n");
    symlink("a.txt", "link.txt");
    mkdir("sub", 0755); touch("sub/inner.txt", "i\n"); touch("sub/relative.txt", "r\n");
    symlink("sub", "dirlink");
    mkdir("listed", 0755); touch("listed/one.txt", "1\n");
    mkdir("bulk", 0755); touch("bulk/one.txt", "1\n");
    mkdir("byid", 0755); touch("byid/secret.txt", "s\n");
    const char *children[] = {"child-fork.txt", "child-spawn.txt", "child-sh.txt", "child-env.txt"};
    for (int i = 0; i < 4; i++) touch(children[i], "c\n");
    touch("opener.py", "#!/usr/bin/env python3\nimport sys\nopen(sys.argv[1]).close()\n");
    chmod("opener.py", 0755);
    printf("pid %d\n", getpid());
    for (int round = 1; round <= 2; round++) {
        printf("round %d\n", round);
        once();
    }
    return 0;
}
