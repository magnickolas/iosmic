#include "resampler.h"
#include <stdexcept>
extern "C" {
#include <libswresample/swresample.h>
}

AudioResampler::AudioResampler(const resample_info &dst,
                               const resample_info &src) {
  context = nullptr;
  input_freq = src.samples_per_sec;
  output_size = 0;
  output_ch = dst.layout.nb_channels;
  output_freq = dst.samples_per_sec;
  output_format = dst.format;
  output_planes = av_sample_fmt_is_planar(dst.format) ? output_ch : 1;

  int nb_ch = src.layout.nb_channels;
  av_channel_layout_default(&input_ch_layout, nb_ch);
  av_channel_layout_default(&output_ch_layout, output_ch);

  swr_alloc_set_opts2(&context, &output_ch_layout, output_format,
                      dst.samples_per_sec, &input_ch_layout, src.format,
                      src.samples_per_sec, 0, NULL);

  if (!context) {
    throw std::runtime_error("swr_alloc_set_opts2 failed");
  }

  AVChannelLayout test_ch = AV_CHANNEL_LAYOUT_MONO;
  if (av_channel_layout_compare(&input_ch_layout, &test_ch) == 0 &&
      output_ch > 1) {
    const double matrix[MAX_AUDIO_CHANNELS][MAX_AUDIO_CHANNELS] = {
        {1},
        {1, 1},
        {1, 1, 0},
        {1, 1, 1, 1},
        {1, 1, 1, 0, 1},
        {1, 1, 1, 1, 1, 1},
        {1, 1, 1, 0, 1, 1, 1},
        {1, 1, 1, 0, 1, 1, 1, 1},
    };
    if (swr_set_matrix(context, matrix[output_ch - 1], 1) < 0) {
      throw std::runtime_error("swr_set_matrix failed");
    }
  }

  if (swr_init(context) != 0) {
    throw std::runtime_error("swr_init failed");
  }
}

AudioResampler::~AudioResampler() {
  if (context)
    swr_free(&context);
  if (output_buffer[0])
    av_freep(&output_buffer[0]);
}

std::pair<uint8_t *, int> AudioResampler::resample(AVFrame *frame) {
  int64_t delay = swr_get_delay(context, input_freq);
  int estimated = (int)av_rescale_rnd(delay + (int64_t)frame->nb_samples,
                                      (int64_t)output_freq, (int64_t)input_freq,
                                      AV_ROUND_UP);
  if (estimated > output_size) {
    if (output_buffer[0])
      av_freep(&output_buffer[0]);
    av_samples_alloc(output_buffer, NULL, output_ch, estimated, output_format,
                     0);
    output_size = estimated;
  }
  int ret = swr_convert(context, output_buffer, output_size,
                        (const uint8_t **)frame->data, frame->nb_samples);
  if (ret < 0) {
    throw std::runtime_error("swr_convert failed");
  }
  return std::make_pair(output_buffer[0], ret);
}

AVSampleFormat AudioResampler::get_output_format() const {
  return output_format;
}

int AudioResampler::get_output_ch() const { return output_ch; }

uint32_t AudioResampler::get_output_freq() const { return output_freq; }
