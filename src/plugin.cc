#include <cstdio>
#include <iostream>
extern "C" {
#include <libavutil/samplefmt.h>
#include <libswresample/swresample.h>
}
#include <stdlib.h>

#include "buffer_util.h"
#include "common.h"
#include "connection.h"
#include "ffmpeg_decode.h"
#include "loopback.h"
#include "net.h"
#include "plugin.h"
#include "settings.h"
#include <memory>
#include <obs/media-io/audio-resampler.h>
#include <sys/stat.h>
#include <sys/types.h>

struct ctx_t {
  std::unique_ptr<audio_resampler_t, decltype(&audio_resampler_destroy)>
      resampler;
  socket_t sock = INVALID_SOCKET;
  std::unique_ptr<Decoder> decoder;
  AVFrame *frame;
  std::vector<AlsaLoopback> loopbacks;

  ctx_t(std::vector<std::string> &&loopbacks)
      : resampler(nullptr, &audio_resampler_destroy), decoder(nullptr),
        frame(nullptr) {
    for (auto &loopback : loopbacks) {
      this->loopbacks.emplace_back(std::move(loopback));
    }
  }

  ~ctx_t() {
    if (sock != INVALID_SOCKET)
      net_close(sock);
  }
};

static socket_t get_connection() {
  socket_t socket = INVALID_SOCKET;

  if (g_settings.connection == radios::CB_RADIO_IOS) {
    socket = CheckiOSDevices(g_settings.port);
    printf("ios socket=%d\n", socket);
    if (socket <= 0)
      socket = INVALID_SOCKET;
  } else {
    const char *err;
    socket = Connect(g_settings.ip.c_str(), g_settings.port, &err);
    if (socket == INVALID_SOCKET) {
      errprint("Failed to connect to %s:%d: %s", g_settings.ip.c_str(),
               g_settings.port, err);
    }
  }
  return socket;
}

#define MAXCONFIG 1024
#define MAXPACKET 1024 * 1024
static DataPacket *read_frame(Decoder *decoder, socket_t sock,
                              int *has_config) {
  uint8_t header[HEADER_SIZE];
  uint8_t config[MAXCONFIG];
  size_t r;
  size_t len, config_len = 0;
  uint64_t pts;

AGAIN:
  r = net_recv_all(sock, header, HEADER_SIZE);
  if (r != HEADER_SIZE) {
    elog("read header recv returned %ld", r);
    return NULL;
  }

  pts = buffer_read64be(header);
  len = buffer_read32be(&header[8]);

  if (pts == NO_PTS) {
    if (config_len != 0) {
      elog("double config ???");
      return NULL;
    }

    if ((int)len == -1) {
      elog("stop/error from app side");
      return NULL;
    }

    if (len == 0 || len > MAXCONFIG) {
      elog("config packet too large at %ld!", len);
      return NULL;
    }

    r = net_recv_all(sock, config, len);
    if (r != len) {
      elog("read config recv returned %ld", r);
      return NULL;
    }

    printf("have config: %ld", len);
    config_len = len;
    *has_config = 1;
    goto AGAIN;
  }

  if (len == 0 || len > MAXPACKET) {
    elog("data packet too large at %ld!", len);
    return NULL;
  }

  DataPacket *data_packet = decoder->pull_empty_packet(config_len + len);
  uint8_t *p = data_packet->data;
  if (config_len) {
    memcpy(p, config, config_len);
    p += config_len;
  }

  r = net_recv_all(sock, p, len);
  if (r != len) {
    elog("read_frame: read %ld bytes wanted %ld", r, len);
    decoder->push_empty_packet(data_packet);
    return NULL;
  }

  data_packet->pts = pts;
  data_packet->used = config_len + len;
  return data_packet;
}

static void play_audio_to_pipe(ctx_t &ctx) {
  uint8_t *output[1];
  uint64_t ts_offset;
  uint32_t out_frames;
  audio_resampler_resample(ctx.resampler.get(), output, &out_frames, &ts_offset,
                           ctx.frame->data, ctx.frame->nb_samples);
  auto out_data = (uint8_t *)output[0];
  for (auto &loopback : ctx.loopbacks) {
    loopback.push(out_data);
  }
  /* fwrite(out_data, 1, out_frames * 4, ctx.fp.get()); */
  /* FILE *debf = fopen("/tmp/deb.pcm", "ab"); */
  /* fwrite(out_data, 1, out_frames * 4, debf); */
  /* fclose(debf); */
}

static bool do_audio_frame(ctx_t &ctx) {
  FFMpegDecoder *decoder = (FFMpegDecoder *)ctx.decoder.get();

  int has_config = 0;
  bool got_output;
  DataPacket *data_packet = read_frame(decoder, ctx.sock, &has_config);
  if (!data_packet)
    return false;

  if (decoder->failed) {
  FAILED:
    dlog("discarding audio frame.. decoder failed");
    decoder->push_empty_packet(data_packet);
    return true;
  }

  if (has_config || !decoder->ready) {
    if (decoder->ready) {
      ilog("unexpected audio config change while decoder is init'd");
      decoder->failed = true;
      goto FAILED;
    }

    if (decoder->init(data_packet->data, AV_CODEC_ID_AAC) < 0) {
      elog("could not initialize AAC decoder");
      decoder->failed = true;
      goto FAILED;
    }

    decoder->push_empty_packet(data_packet);
    return true;
  }

  if (!decoder->decode_audio(data_packet, &got_output)) {
    elog("error decoding audio");
    decoder->failed = true;
    goto FAILED;
  }

  if (got_output) {
    ctx.frame = decoder->frame;
    play_audio_to_pipe(ctx);
  }

  decoder->push_empty_packet(data_packet);
  return true;
}

void *run_audio_thread(void *arg) {
  (void)arg;
  const char *audio_req = AUDIO_REQ;

  ctx_t ctx{{"hw:0"}};
  struct resample_info src_info;
  src_info.format = AUDIO_FORMAT_FLOAT_PLANAR;
  src_info.samples_per_sec = 44100;
  src_info.speakers = SPEAKERS_MONO;

  struct resample_info dst_info;
  dst_info.format = AUDIO_FORMAT_32BIT;
  dst_info.samples_per_sec = 44100;
  dst_info.speakers = SPEAKERS_MONO;

  ctx.resampler.reset(audio_resampler_create(&dst_info, &src_info));

  ctx.decoder.reset(new FFMpegDecoder());

  if ((ctx.sock = get_connection()) == INVALID_SOCKET) {
    elog("audio: get_connection failed");
    return nullptr;
  }

  if (net_send_all(ctx.sock, audio_req, CSTR_LEN(AUDIO_REQ)) <= 0) {
    elog("send(/audio) failed");
    return nullptr;
  }

  printf("audio_thread start\n");
  while (a_running) {
    if (!do_audio_frame(ctx)) {
      break;
    }
  }

  ilog("audio_thread end");
  return NULL;
}
