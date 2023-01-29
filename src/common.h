#pragma once

#include "settings.h"
#define APP_VER_INT 182
#define APP_VER_STR "1.8.2"

#define MSG_ERROR(str) ShowError("Error", str)
#define MSG_LASTERROR(str) ShowError(str, strerror(errno))
void ShowError(const char *, const char *);

#define ADB_LOCALHOST_IP "127.0.0.1"

#define AUDIO_REQ "GET /v1/audio.2"

#define CSTR_LEN(x) (sizeof(x) - 1)

#define errprint(...) fprintf(stderr, __VA_ARGS__)
#define dbgprint errprint

#ifndef FALSE
#define FALSE 0
#endif

#ifndef TRUE
#define TRUE 1
#endif

inline int a_running = 1;
inline struct settings g_settings;
