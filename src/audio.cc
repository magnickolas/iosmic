#include "audio.h"
#include "buffer_util.h"
#include "common.h"
#include "connection.h"
#include "emitter.h"
#include "ffmpeg_decode.h"
#include "ios_socket.h"
#include "loopback.h"
#include "net.h"
#include "resampler.h"
#include "settings.h"
#include <cstdio>
#include <iostream>
#include <memory>
#include <stdlib.h>
#include <sys/stat.h>
#include <sys/types.h>
extern "C" {
#include <libavutil/samplefmt.h>
#include <libswresample/swresample.h>
}

void *run_audio_thread(void *) {
  IOSSocket sock{g_settings.ip, g_settings.port, g_settings.connection};
  AudioResampler resampler(
      {
          .samples_per_sec = 44100,
          .layout = AV_CHANNEL_LAYOUT_MONO,
          .format = AV_SAMPLE_FMT_S32,

      },
      {
          .samples_per_sec = 44100,
          .layout = AV_CHANNEL_LAYOUT_MONO,
          .format = AV_SAMPLE_FMT_FLTP,
      }

  );
  AudioEmitter emitter{
      sock, {"hw:0"}, std::move(resampler), std::make_unique<FFMpegDecoder>()};

  printf("audio_thread start\n");
  while (a_running) {
    emitter.emit();
  }

  ilog("audio_thread end");
  return NULL;
}
