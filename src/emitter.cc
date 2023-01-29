#include "emitter.h"
#include "buffer_util.h"
#include "audio.h"
#include <stdexcept>

static snd_pcm_format_t
snd_pcm_format_from_sample_format(AVSampleFormat format) {
  switch (format) {
  case AV_SAMPLE_FMT_U8:
    return SND_PCM_FORMAT_U8;
  case AV_SAMPLE_FMT_S16:
    return SND_PCM_FORMAT_S16;
  case AV_SAMPLE_FMT_S32:
    return SND_PCM_FORMAT_S32;
  case AV_SAMPLE_FMT_FLT:
    return SND_PCM_FORMAT_FLOAT;
  case AV_SAMPLE_FMT_DBL:
    return SND_PCM_FORMAT_FLOAT64;
  default:
    throw std::runtime_error("unsupported sample format");
  }
}

AudioEmitter::AudioEmitter(const IOSSocket &sock,
                           std::vector<std::string> &&loopback_names,
                           AudioResampler &&resampler,
                           std::unique_ptr<FFMpegDecoder> decoder)
    : sock(sock), resampler(resampler), decoder(std::move(decoder)) {
  for (auto &loopback : loopback_names) {
    this->loopbacks.emplace_back(
        std::move(loopback), resampler.get_output_freq(),
        resampler.get_output_ch(),
        snd_pcm_format_from_sample_format(resampler.get_output_format()));
  }
  if (net_send_all(sock.get(), AUDIO_REQ, CSTR_LEN(AUDIO_REQ)) <= 0) {
    throw std::runtime_error("send(/audio) failed");
  }
}

void AudioEmitter::emit(AVFrame *frame) {
  auto [out_data, out_frames] = resampler.resample(frame);
  for (auto &loopback : loopbacks) {
    loopback.push(out_data, out_frames);
  }
}

DataPacket *AudioEmitter::read_frame(int &has_config) {
  constexpr size_t HEADER_SIZE = 12;
  constexpr size_t MAXCONFIG = 1024;
  constexpr size_t MAXPACKET = 1024 * 1024;
  uint8_t header[HEADER_SIZE];
  uint8_t config[MAXCONFIG];
  size_t r;
  size_t len, config_len = 0;
  uint64_t pts;

  do {
    r = net_recv_all(sock.get(), header, HEADER_SIZE);
    if (r != HEADER_SIZE) {
      throw std::runtime_error("read header recv returned " +
                               std::to_string(r));
    }
    pts = buffer_read64be(header);
    len = buffer_read32be(&header[8]);
    if (pts == NO_PTS) {
      if (config_len != 0) {
        throw std::runtime_error("config packet received twice");
      }
      if ((int)len == -1) {
        throw std::runtime_error("stop/error from app side");
      }
      if (len == 0 || len > MAXCONFIG) {
        throw std::runtime_error("config packet too large at " +
                                 std::to_string(len));
      }
      r = net_recv_all(sock.get(), config, len);
      if (r != len) {
        throw std::runtime_error("read config recv returned " +
                                 std::to_string(r));
      }
      config_len = len;
      has_config = 1;
    }
  } while (pts == NO_PTS);

  if (len == 0 || len > MAXPACKET) {
    throw std::runtime_error("data packet too large at " + std::to_string(len));
  }

  DataPacket *data_packet = decoder->pull_empty_packet(config_len + len);
  uint8_t *p = data_packet->data;
  if (config_len) {
    memcpy(p, config, config_len);
    p += config_len;
  }
  r = net_recv_all(sock.get(), p, len);
  if (r != len) {
    elog("read data recv returned %ld, wanted %ld", r, len);
    decoder->push_empty_packet(data_packet);
    return nullptr;
  }
  data_packet->pts = pts;
  data_packet->used = config_len + len;
  return data_packet;
}

void AudioEmitter::emit() {
  int has_config = 0;
  bool got_output;
  DataPacket *data_packet = read_frame(has_config);
  if (!data_packet) {
    throw std::runtime_error("no data packet");
  }
  if (decoder->failed) {
    decoder->push_empty_packet(data_packet);
    throw std::runtime_error("decoder failed");
  }
  if (has_config || !decoder->ready) {
    if (decoder->ready) {
      decoder->push_empty_packet(data_packet);
      throw std::runtime_error(
          "unexpected audio config change while decoder is initialized");
    }
    if (decoder->init(data_packet->data, AV_CODEC_ID_AAC) < 0) {
      decoder->push_empty_packet(data_packet);
      throw std::runtime_error("could not initialize AAC decoder");
    }
    decoder->push_empty_packet(data_packet);
    return;
  }
  if (!decoder->decode_audio(data_packet, &got_output)) {
    decoder->failed = true;
    decoder->push_empty_packet(data_packet);
    throw std::runtime_error("decoder failed");
  }
  if (got_output) {
    emit(decoder->frame);
  }
  decoder->push_empty_packet(data_packet);
}
