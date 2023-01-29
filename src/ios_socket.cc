#include "ios_socket.h"
#include "common.h"
#include "connection.h"
#include "settings.h"
#include <stdexcept>

IOSSocket::IOSSocket(const std::string &ip, int port, radios connection_type) {
  socket = INVALID_SOCKET;

  if (connection_type == radios::CB_RADIO_IOS) {
    socket = CheckiOSDevices(port);
    if (socket <= 0) {
      socket = INVALID_SOCKET;
      throw std::runtime_error("No iOS device found");
    }
  } else {
    const char *err;
    socket = Connect(ip.c_str(), port, &err);
    if (socket == INVALID_SOCKET) {
      throw std::runtime_error(err);
    }
  }
}

IOSSocket::~IOSSocket() {
  if (socket != INVALID_SOCKET) {
    net_close(socket);
  }
}

socket_t IOSSocket::get() const { return socket; }
