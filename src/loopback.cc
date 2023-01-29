#include "loopback.h"
#include <stdexcept>
#include <string>

const size_t AlsaLoopback::bufsize = 4096;
const int AlsaLoopback::rate = 44100;
const int AlsaLoopback::nchannels = 1;
const size_t AlsaLoopback::buf_frames =
    AlsaLoopback::bufsize / 4 / AlsaLoopback::nchannels;
const snd_pcm_format_t AlsaLoopback::format = SND_PCM_FORMAT_S32_LE;

AlsaLoopback::AlsaLoopback(std::string &&device, int _rate, int _nchannels,
                           snd_pcm_format_t _format) {
  int err;
  if ((err = snd_pcm_open(&playback_handle, device.c_str(),
                          SND_PCM_STREAM_PLAYBACK, 0)) < 0) {
    throw std::runtime_error("cannot open audio device " + std::string(device) +
                             " (" + snd_strerror(err) + ")");
  }
  if ((err = snd_pcm_set_params(playback_handle, _format,
                                SND_PCM_ACCESS_RW_INTERLEAVED, _nchannels,
                                _rate, 0, 50000)) < 0) {
    throw std::runtime_error("cannot set audio parameters (" +
                             std::string(snd_strerror(err)) + ")");
  }
}

AlsaLoopback::~AlsaLoopback() { snd_pcm_close(playback_handle); }

void AlsaLoopback::push(uint8_t *buf, int buf_frames) {
  int err;
  if ((err = snd_pcm_writei(playback_handle, buf, buf_frames)) != buf_frames) {
    throw std::runtime_error("write to audio interface failed" +
                             std::string(snd_strerror(err)));
  }
}
