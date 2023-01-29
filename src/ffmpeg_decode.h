#pragma once

#ifdef __cplusplus
extern "C" {
#endif

#include <obs/obs.h>

#ifdef _MSC_VER
#pragma warning(push)
#pragma warning(disable : 4244)
#pragma warning(disable : 4204)
#endif

#include <libavcodec/avcodec.h>

#ifdef _MSC_VER
#pragma warning(pop)
#endif

#ifdef __cplusplus
}
#endif

#include "decoder_interface.h"

struct FFMpegDecoder : Decoder {
  const AVCodec *codec;
  AVCodecContext *decoder;
  AVPacket *packet;
  AVFrame *frame;
  bool catchup;
  bool b_frame_check;

  FFMpegDecoder(void) {
    decoder = NULL;
    packet = NULL;
    frame = NULL;
    catchup = false;
    b_frame_check = false;
  }

  ~FFMpegDecoder(void);

  int init(uint8_t *header, enum AVCodecID id);

  bool decode_audio(DataPacket *, bool *got_output);

  DataPacket *pull_empty_packet(size_t size);
  void push_ready_packet(DataPacket *);
};
