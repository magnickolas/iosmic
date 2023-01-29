#pragma once

#include <obs/obs-module.h>

#define xlog(log_level, format, ...)                                           \
  blog(log_level, "[DroidCamOBS] " format, ##__VA_ARGS__)

#ifdef DEBUG
#define dlog(format, ...) xlog(LOG_INFO, format, ##__VA_ARGS__)
#else
#define dlog(format, ...) /* */
#endif
#define ilog(format, ...) xlog(LOG_INFO, format, ##__VA_ARGS__)
#define elog(format, ...) xlog(LOG_WARNING, format, ##__VA_ARGS__)

#define HEADER_SIZE 12
#define NO_PTS UINT64_C(~0)
#define ARRAY_LEN(a) (sizeof(a) / sizeof(a[0]))

void *run_audio_thread(void *);
