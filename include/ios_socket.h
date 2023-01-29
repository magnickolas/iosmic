#pragma once
#include "net.h"
#include "settings.h"
#include <string>

class IOSSocket {
public:
  IOSSocket(const std::string &ip, int port, radios connection_type);
  ~IOSSocket();
  socket_t get() const;

private:
  socket_t socket;
};
