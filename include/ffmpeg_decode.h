#pragma once

#ifdef __cplusplus
extern "C" {
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
  const AVCodec *codec = nullptr;
  AVCodecContext *decoder = nullptr;
  AVPacket *packet = nullptr;
  AVFrame *frame = nullptr;
  bool catchup = false;
  bool b_frame_check = false;

  ~FFMpegDecoder();

  int init(uint8_t *header, enum AVCodecID id);

  bool decode_audio(DataPacket *, bool *got_output);

  DataPacket *pull_empty_packet(size_t size);
  void push_ready_packet(DataPacket *);
};
