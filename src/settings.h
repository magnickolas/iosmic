// Copyright (C) 2020 github.com/dev47apps
#ifndef _SETTINGS_H_
#define _SETTINGS_H_

#include <string>
enum class radios { CB_RADIO_WIFI, CB_RADIO_IOS };

struct settings {
  std::string ip;
  int port;
  int audio;
  radios connection; // Connection type

  int adb_auto_start;
  int confirm_close;
};

void LoadSettings(struct settings *settings);
void SaveSettings(struct settings *settings);

#define NO_ERROR 0
#define ERROR_NO_DEVICES -1
#define ERROR_LOADING_DEVICES -2
#define ERROR_ADDING_FORWARD -3
#define ERROR_DEVICE_OFFLINE -4
#define ERROR_DEVICE_NOTAUTH -5
int CheckAdbDevices(int port);
int CheckiOSDevices(int port);

void AdbErrorPrint(int rc);
void iOSErrorPrint(int rc);

#endif
