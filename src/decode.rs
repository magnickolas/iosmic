use anyhow::{Context, Result};
use symphonia::core::audio::GenericAudioBufferRef;
use symphonia::core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, well_known::CODEC_ID_AAC,
};
use symphonia::core::packet::Packet;
use symphonia::core::units::{Duration, Timestamp};

pub struct AacDecoder {
    decoder: Box<dyn AudioDecoder>,
    sample_rate: u32,
    channels: usize,
}

impl AacDecoder {
    pub fn new(config_data: &[u8]) -> Result<Self> {
        let (sample_rate, channels) = parse_aac_config(config_data)?;

        let mut codec_params = AudioCodecParameters::new();
        codec_params
            .for_codec(CODEC_ID_AAC)
            .with_sample_rate(sample_rate)
            .with_extra_data(config_data.into());

        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&codec_params, &AudioDecoderOptions::default())
            .context("Failed to create AAC decoder")?;

        Ok(Self {
            decoder,
            sample_rate,
            channels,
        })
    }

    pub fn decode(&mut self, data: &[u8], pts: u64) -> Result<Vec<f32>> {
        let packet = Packet::new(0, Timestamp::new(pts as i64), Duration::ZERO, data);
        let decoded = self.decoder.decode(&packet).context("AAC decode failed")?;
        Ok(audio_buf_to_f32(&decoded))
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> usize {
        self.channels
    }
}

fn parse_aac_config(header: &[u8]) -> Result<(u32, usize)> {
    const AAC_FREQUENCIES: &[u32] = &[
        96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000,
    ];

    anyhow::ensure!(header.len() >= 2, "AAC config too short");

    let sr_idx = ((((header[0] & 0x07) as u16) << 1) | ((header[1] as u16) >> 7)) as usize;
    anyhow::ensure!(
        sr_idx < AAC_FREQUENCIES.len(),
        "Invalid AAC sample rate index: {}",
        sr_idx
    );

    let channels = ((header[1] >> 3) & 0xF) as usize;
    anyhow::ensure!(channels > 0, "Unsupported AAC channel configuration: 0");
    Ok((AAC_FREQUENCIES[sr_idx], channels))
}

fn audio_buf_to_f32(buf: &GenericAudioBufferRef<'_>) -> Vec<f32> {
    let mut out = Vec::with_capacity(buf.samples_interleaved());
    buf.copy_to_vec_interleaved::<f32>(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::parse_aac_config;

    fn audio_specific_config(object_type: u8, sample_rate_idx: u8, channels: u8) -> [u8; 2] {
        [
            (object_type << 3) | (sample_rate_idx >> 1),
            ((sample_rate_idx & 1) << 7) | (channels << 3),
        ]
    }

    #[test]
    fn parses_sample_rate_independent_of_object_type_low_bit() {
        let config = audio_specific_config(3, 4, 2);

        let (sample_rate, channels) = parse_aac_config(&config).unwrap();

        assert_eq!(sample_rate, 44100);
        assert_eq!(channels, 2);
    }

    #[test]
    fn rejects_channel_config_zero() {
        let config = audio_specific_config(2, 4, 0);

        let err = parse_aac_config(&config).unwrap_err();

        assert!(err.to_string().contains("channel configuration"), "{err:?}");
    }
}
