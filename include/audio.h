#pragma once

#define NO_PTS UINT64_C(~0)
#define ARRAY_LEN(a) (sizeof(a) / sizeof(a[0]))

void *run_audio_thread(void *);
