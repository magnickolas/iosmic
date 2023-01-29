/*
        Copyright (C) 2014 by Hugh Bailey <obs.jim@gmail.com>
        Copyright (C) 2021 DEV47APPS, github.com/dev47apps

        This program is free software: you can redistribute it and/or modify
        it under the terms of the GNU General Public License as published by
        the Free Software Foundation, either version 2 of the License, or
        (at your option) any later version.

        This program is distributed in the hope that it will be useful,
        but WITHOUT ANY WARRANTY; without even the implied warranty of
        MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
        GNU General Public License for more details.

        You should have received a copy of the GNU General Public License
        along with this program.  If not, see <http://www.gnu.org/licenses/>.
*/

#include "ffmpeg_decode.h"
#include "plugin.h"
#include <libavutil/channel_layout.h>
#include <obs/obs-ffmpeg-compat.h>

int FFMpegDecoder::init(uint8_t *header, enum AVCodecID id) {
  int ret;

  codec = avcodec_find_decoder(id);
  if (!codec)
    return -1;

  decoder = avcodec_alloc_context3(codec);
  decoder->opaque = this;

  if (id == AV_CODEC_ID_AAC) {
    // https://wiki.multimedia.cx/index.php/MPEG-4_Audio
    static int aac_frequencies[] = {96000, 88200, 64000, 48000, 44100, 32000,
                                    24000, 22050, 16000, 12000, 11025, 8000};
    if (!header) {
      elog("missing AAC header required to init decoder");
      return -1;
    }

    int sr_idx = ((header[0] << 1) | (header[1] >> 7)) & 0x1F;
    if (sr_idx < 0 || sr_idx >= (int)ARRAY_LEN(aac_frequencies)) {
      elog("failed to parse AAC header, sr_idx=%d [0x%2x 0x%2x]", sr_idx,
           header[0], header[1]);
      return -1;
    }

    decoder->sample_rate = aac_frequencies[sr_idx];
    decoder->profile = FF_PROFILE_AAC_LOW;

    const int channels = (header[1] >> 3) & 0xF;
#if LIBAVCODEC_VERSION_INT < AV_VERSION_INT(59, 24, 100)
    decoder->channels = channels;
    switch (decoder->channels) {
    case 1:
      decoder->channel_layout = AV_CH_LAYOUT_MONO;
      break;
    case 2:
      decoder->channel_layout = AV_CH_LAYOUT_STEREO;
      break;
    default:
      decoder->channel_layout = 0; // unknown
    }
#else
    av_channel_layout_default(&decoder->ch_layout, channels);
#endif

    ilog("audio: sample_rate=%d channels=%d", decoder->sample_rate,
         decoder->ch_layout.nb_channels);
  }

  ret = avcodec_open2(decoder, codec, NULL);
  if (ret < 0) {
    return ret;
  }

  // if (codec->capabilities & CODEC_CAP_TRUNC)
  // 	decoder->flags |= CODEC_FLAG_TRUNC;
  decoder->flags |= AV_CODEC_FLAG_LOW_DELAY;
  decoder->flags2 |= AV_CODEC_FLAG2_FAST;
  // decoder->flags2 |= AV_CODEC_FLAG2_CHUNKS;
  decoder->thread_type = FF_THREAD_SLICE;

  frame = av_frame_alloc();
  if (!frame)
    return -1;

  packet = av_packet_alloc();
  if (!packet)
    return -1;

  ready = true;
  return 0;
}

FFMpegDecoder::~FFMpegDecoder(void) {
  if (frame)
    av_frame_free(&frame);

  if (packet)
    av_packet_free(&packet);

  if (decoder)
    avcodec_free_context(&decoder);
}

static inline enum audio_format convert_sample_format(int f) {
  switch (f) {
  case AV_SAMPLE_FMT_S16:
    return AUDIO_FORMAT_16BIT;
  case AV_SAMPLE_FMT_S32:
    return AUDIO_FORMAT_32BIT;
  case AV_SAMPLE_FMT_U8:
    return AUDIO_FORMAT_U8BIT;
  case AV_SAMPLE_FMT_FLT:
    return AUDIO_FORMAT_FLOAT;
  case AV_SAMPLE_FMT_U8P:
    return AUDIO_FORMAT_U8BIT_PLANAR;
  case AV_SAMPLE_FMT_S16P:
    return AUDIO_FORMAT_16BIT_PLANAR;
  case AV_SAMPLE_FMT_S32P:
    return AUDIO_FORMAT_32BIT_PLANAR;
  case AV_SAMPLE_FMT_FLTP:
    return AUDIO_FORMAT_FLOAT_PLANAR;
  default:;
  }

  return AUDIO_FORMAT_UNKNOWN;
}

static inline enum speaker_layout convert_speaker_layout(int channels) {
  switch (channels) {
  case 1:
    return SPEAKERS_MONO;
  case 2:
    return SPEAKERS_STEREO;
  case 3:
    return SPEAKERS_2POINT1;
  case 4:
    return SPEAKERS_4POINT0;
  case 5:
    return SPEAKERS_4POINT1;
  case 6:
    return SPEAKERS_5POINT1;
  case 8:
    return SPEAKERS_7POINT1;
  default:
    return SPEAKERS_UNKNOWN;
  }
}

DataPacket *FFMpegDecoder::pull_empty_packet(size_t size) {
  size_t new_size = size + INPUT_BUFFER_PADDING_SIZE;
  DataPacket *packet = Decoder::pull_empty_packet(new_size);
  memset(packet->data, 0, new_size);
  return packet;
}

void FFMpegDecoder::push_ready_packet(DataPacket *packet) {
  if (catchup) {
    if (decodeQueue.items.size() > 0) {
      receiveQueue.add_item(packet);
      return;
    }

    // Discard P/B frames and continue on anything higher
    if (codec->id == AV_CODEC_ID_H264) {
      int nalType = packet->data[2] == 1 ? (packet->data[3] & 0x1f)
                                         : (packet->data[4] & 0x1f);
      if (nalType < 5) {
        dlog("discard non-keyframe");
        receiveQueue.add_item(packet);
        return;
      }
    }

    ilog("decoder catchup: decodeQueue: %ld recieveQueue: %ld",
         decodeQueue.items.size(), receiveQueue.items.size());
    catchup = false;
  }

  decodeQueue.add_item(packet);

  if (codec->id == AV_CODEC_ID_H264 && decodeQueue.items.size() > 25) {
    catchup = true;
  }
  // ((uint64_t)plugin->obs_audio_frame.frames * MILLI_SEC /
  // (uint64_t)plugin->obs_audio_frame.samples_per_sec) At 44100HZ, 1 AAC Frame
  // = 23ms
  else if (codec->id == AV_CODEC_ID_AAC &&
           decodeQueue.items.size() > (1000 / 23)) {
    catchup = true;
  }
}

bool FFMpegDecoder::decode_audio(DataPacket *data_packet, bool *got_output) {
  int ret;
  *got_output = false;

  packet->data = data_packet->data;
  packet->size = data_packet->used;
  packet->pts =
      (data_packet->pts == NO_PTS) ? AV_NOPTS_VALUE : data_packet->pts;

  ret = avcodec_send_packet(decoder, packet);
  if (ret == 0) {
    ret = avcodec_receive_frame(decoder, frame);
    if (ret == 0)
      goto GOT_FRAME;
  }

  return ret == AVERROR(EAGAIN);

GOT_FRAME:
  *got_output = true;
  return true;
}
