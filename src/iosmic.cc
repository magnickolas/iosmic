/* DroidCam & DroidCamX (C) 2010-2021
 * https://github.com/dev47apps
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.
 */

#include <pthread.h>
#include <signal.h>

#include "common.h"
#include "connection.h"
/* #include "decoder.h" */
#include "audio.h"
#include "settings.h"

void sig_handler(__attribute__((__unused__)) int sig) {
  a_running = 0;
  return;
}

void ShowError(const char *title, const char *msg) {
  errprint("%s: %s\n", title, msg);
}

static inline void usage(const char *name) {
  fprintf(stderr,
          "Usage: \n"
          " %s <ip> <port>\n"
          "   Connect via ip\n"
          "\n"
          " %s [options] usb <port>\n"
          "   Connect via usbmuxd to iDevice\n"
          "\n",
          name, name);
}

static void parse_args(int argc, char *argv[]) {
  if (argc - 1 == 2) {
    g_settings.ip = argv[1];
    g_settings.port = strtoul(argv[2], NULL, 10);

    if (g_settings.ip == "usb") {
      g_settings.connection = radios::CB_RADIO_IOS;
    } else {
      printf("ip %s\n", g_settings.ip.c_str());
      g_settings.connection = radios::CB_RADIO_WIFI;
    }

    return;
  }

  usage(argv[0]);
  exit(1);
}

int main(int argc, char *argv[]) {
  parse_args(argc, argv);

  pthread_t audio_thread;
  pthread_create(&audio_thread, NULL, run_audio_thread, NULL);

  /* signal(SIGINT, sig_handler); */
  /* signal(SIGHUP, sig_handler); */
  /*  */
  /* sig_handler(SIGHUP); */
  pthread_join(audio_thread, NULL);

  dbgprint("exit\n");
  return 0;
}
