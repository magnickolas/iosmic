#pragma once
#include <alsa/asoundlib.h>
#include <string>

class AlsaLoopback {
public:
  static const size_t bufsize;

private:
  static const int rate;
  static const int nchannels;
  static const snd_pcm_format_t format;
  static const size_t buf_frames;

public:
  AlsaLoopback(std::string &&device, int rate, int nchannels,
               snd_pcm_format_t format);
  ~AlsaLoopback();
  void push(uint8_t *buf, int buf_frames);

private:
  snd_pcm_t *playback_handle;
};
