#pragma once

#include "settings.h"

#define MSG_ERROR(str) ShowError("Error", str)
#define MSG_LASTERROR(str) ShowError(str, strerror(errno))
void ShowError(const char *, const char *);

#define AUDIO_REQ "GET /v1/audio.2"

#define CSTR_LEN(x) (sizeof(x) - 1)

#define errprint(...) fprintf(stderr, __VA_ARGS__)
#define dbgprint errprint

#define xlog(log_level, format, ...) printf(log_level format, ##__VA_ARGS__)
#define ilog(format, ...) xlog("INFO", format, ##__VA_ARGS__)
#define elog(format, ...) xlog("WARNING", format, ##__VA_ARGS__)
#define dlog(format, ...) xlog("DEBUG", format, ##__VA_ARGS__)

inline int a_running = 1;
inline struct settings g_settings;
