#pragma once

#include <cstdint>
#include <optional>
#include <utility>
extern "C" {
#include <libavutil/channel_layout.h>
#include <libavutil/frame.h>
#include <libavutil/samplefmt.h>
#include <libswresample/swresample.h>
}

class AudioResampler {
  static constexpr size_t MAX_AV_PLANES = 8;
  static constexpr size_t MAX_AUDIO_CHANNELS = 8;

public:
  struct resample_info {
    uint32_t samples_per_sec;
    AVChannelLayout layout;
    AVSampleFormat format;
  };
  AudioResampler(const resample_info &dst, const resample_info &src);
  ~AudioResampler();
  std::pair<uint8_t *, int> resample(AVFrame *frame);

  AVSampleFormat get_output_format() const;
  int get_output_ch() const;
  uint32_t get_output_freq() const;

private:
  SwrContext *context;
  uint32_t input_freq;
  uint32_t output_freq;
  AVSampleFormat input_format;
  AVSampleFormat output_format;
  AVChannelLayout input_ch_layout;
  AVChannelLayout output_ch_layout;
  uint32_t output_ch;
  uint32_t output_planes;
  uint8_t *output_buffer[MAX_AV_PLANES];
  int output_size;
};
