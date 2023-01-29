// Copyright (C) 2021 DEV47APPS, github.com/dev47apps
#ifndef COMMAND_H
#define COMMAND_H

#include "plugin.h"
#include <stdbool.h>
#include <inttypes.h>

#ifdef _WIN32

# include <windows.h>

# define strtok_r strtok_s
# define PATH_SEPARATOR "\\"
# define PRIexitcode "lu"
// <https://stackoverflow.com/a/44383330/1987178>
# ifdef _WIN64
#   define PRIsizet PRIu64
# else
#   define PRIsizet PRIu32
# endif
# define PROCESS_NONE NULL
  typedef HANDLE process_t;
  typedef DWORD exit_code_t;

static inline bool FileExists(const char *path) {
  DWORD dwAttrib = GetFileAttributes(path);
  return (dwAttrib != INVALID_FILE_ATTRIBUTES && !(dwAttrib & FILE_ATTRIBUTE_DIRECTORY));
}
#else

# include <sys/types.h>
# include <ctype.h>
# include <unistd.h>
# define PATH_SEPARATOR "/"
# define PRIsizet "zu"
# define PRIexitcode "d"
# define PROCESS_NONE -1
  typedef pid_t process_t;
  typedef int exit_code_t;

static inline bool FileExists(const char *path) {
  return (access(path, F_OK|R_OK) != -1);
}
#endif

# define NO_EXIT_CODE -1

enum process_result {
    PROCESS_SUCCESS,
    PROCESS_ERROR_GENERIC,
    PROCESS_ERROR_MISSING_BINARY,
};

enum process_result cmd_execute(const char *path, const char *const argv[], process_t *handle, char* output, size_t out_size);

bool cmd_simple_wait(process_t pid, exit_code_t *exit_code);
bool argv_to_string(const char *const *argv, char *buf, size_t bufsize);
bool process_check_success(process_t proc, const char *name);
void process_print_error(enum process_result err, const char *const argv[]);

#endif
