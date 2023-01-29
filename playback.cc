#include <pulse/pulseaudio.h>

// create a pulseaudio sink
void create_sink() {
  pa_sample_spec ss;
  ss.format = PA_SAMPLE_S16LE;
  ss.channels = 2;
  ss.rate = 44100;

  pa_buffer_attr ba;
  ba.maxlength = (uint32_t)-1;
  ba.tlength = (uint32_t)-1;
  ba.prebuf = (uint32_t)-1;
  ba.minreq = (uint32_t)-1;
  ba.fragsize = (uint32_t)-1;

  pa_context *context =
      pa_context_new(pa_threaded_mainloop_get_api(mainloop), "sink");
  pa_context_connect(context, NULL, PA_CONTEXT_NOFLAGS, NULL);
  pa_context_set_state_callback(context, context_state_callback, NULL);
  pa_threaded_mainloop_lock(mainloop);
  while (pa_context_get_state(context) != PA_CONTEXT_READY) {
    pa_threaded_mainloop_wait(mainloop);
  }
  pa_threaded_mainloop_unlock(mainloop);

  pa_stream *stream = pa_stream_new(context, "sink", &ss, NULL);
  pa_stream_connect_playback(stream, NULL, &ba, PA_STREAM_ADJUST_LATENCY, NULL,
                             NULL);
  pa_stream_set_state_callback(stream, stream_state_callback, NULL);
  pa_threaded_mainloop_lock(mainloop);
  while (pa_stream_get_state(stream) != PA_STREAM_READY) {
    pa_threaded_mainloop_wait(mainloop);
  }
  pa_threaded_mainloop_unlock(mainloop);

  pa_threaded_mainloop_lock(mainloop);
  while (true) {
    pa_threaded_mainloop_wait(mainloop);
  }
  pa_threaded_mainloop_unlock(mainloop);
}
