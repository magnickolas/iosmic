#pragma once

#include "resampler.h"

#include "ffmpeg_decode.h"
#include "ios_socket.h"
#include "loopback.h"
#include <memory>
#include <string>
#include <vector>

class AudioEmitter {
public:
  AudioEmitter(const IOSSocket &sock, std::vector<std::string> &&loopbacks,
               AudioResampler &&resampler,
               std::unique_ptr<FFMpegDecoder> decoder);
  ~AudioEmitter() = default;
  void emit();

private:
  void emit(AVFrame *frame);
  DataPacket *read_frame(int &has_config);

  IOSSocket sock;
  AudioResampler resampler;
  std::unique_ptr<FFMpegDecoder> decoder;
  std::vector<AlsaLoopback> loopbacks;
};
