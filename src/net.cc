/*
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

#include "net.h"
#include "plugin.h"
#include <errno.h>
#include <string.h>

#ifdef _WIN32
#include <ws2tcpip.h>
#pragma comment(lib, "ws2_32.lib")
typedef int socklen_t;
#else
#include <arpa/inet.h>
#include <fcntl.h>
#include <netdb.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <unistd.h>
#endif

bool set_nonblock(socket_t sock, int nonblock) {
#ifdef _WIN32
  u_long nb = nonblock;
  return (NO_ERROR == ioctlsocket(sock, FIONBIO, &nb));
#else
  int flags = fcntl(sock, F_GETFL, NULL);
  if (flags < 0) {
    elog("fcntl(): %s", strerror(errno));
    return false;
  }

  if (nonblock)
    flags |= O_NONBLOCK;
  else
    flags &= ~O_NONBLOCK;

  if (fcntl(sock, F_SETFL, flags) < 0) {
    elog("fcntl(): %s", strerror(errno));
    return false;
  }

  return true;
#endif
}

// https://stackoverflow.com/a/2939145
int set_recv_timeout(socket_t sock, int tv_sec) {
#if _WIN32
  DWORD timeout = tv_sec * 1000;
#else
  struct timeval timeout;
  timeout.tv_sec = tv_sec;
  timeout.tv_usec = 0;
#endif

  return setsockopt(sock, SOL_SOCKET, SO_RCVTIMEO, (char *)&timeout,
                    sizeof(timeout));
}

socket_t net_listen(const char *addr, uint16_t port) {
  socket_t sock = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
  if (sock == INVALID_SOCKET) {
    WSAErrno();
    elog("socket(): %s", strerror(errno));
    return INVALID_SOCKET;
  }

  struct sockaddr_in sa = {0};
  sa.sin_family = AF_INET;
  sa.sin_addr.s_addr = inet_addr(addr);
  sa.sin_port = htons(port);

  const int on = 1;
  setsockopt(sock, SOL_SOCKET, SO_REUSEADDR, (char *)&on, sizeof(int));
#if _WIN32
  setsockopt(sock, SOL_SOCKET, SO_EXCLUSIVEADDRUSE, (char *)&on, sizeof(int));
#endif
  set_nonblock(sock, on);

  if (bind(sock, (struct sockaddr *)&sa, sizeof(sa)) < 0) {
    WSAErrno();
    elog("bind(): %s", strerror(errno));
    goto fail;
  }

  if (listen(sock, 8) < 0) {
    WSAErrno();
    elog("listen(): %s", strerror(errno));
  fail:
    net_close(sock);
    return INVALID_SOCKET;
  }

  dlog("created server on %s:%d", addr, net_listen_port(sock));
  return sock;
}

int net_listen_port(socket_t sock) {
  struct sockaddr_in sa;
  socklen_t len = sizeof(sa);
  if (getsockname(sock, (struct sockaddr *)&sa, &len) < 0) {
    WSAErrno();
    elog("getsockname(): %s", strerror(errno));
    return 0;
  }

  return ntohs(sa.sin_port);
}

socket_t net_accept(socket_t sock) { return accept(sock, NULL, 0); }

socket_t net_connect(struct addrinfo *addr, uint16_t port) {
  struct sockaddr *ai_addr = addr->ai_addr;
  void *in_addr;

  switch (ai_addr->sa_family) {
  case AF_INET: {
    struct sockaddr_in *sa = (struct sockaddr_in *)ai_addr;
    in_addr = &(sa->sin_addr);
    sa->sin_port = htons(port);
    break;
  }
  case AF_INET6: {
    struct sockaddr_in6 *sa = (struct sockaddr_in6 *)ai_addr;
    in_addr = &(sa->sin6_addr);
    sa->sin6_port = htons(port);
    break;
  }
  }

#ifdef DEBUG
  char str[INET6_ADDRSTRLEN] = {0};
  inet_ntop(addr->ai_family, in_addr, str, sizeof(str));
  dlog("trying %s", str);
#else
  (void)in_addr;
#endif

  socket_t sock = socket(addr->ai_family, addr->ai_socktype, addr->ai_protocol);
  if (sock == INVALID_SOCKET) {
    WSAErrno();
    elog("socket(): %s", strerror(errno));
    return INVALID_SOCKET;
  }

  struct timeval timeout;
  timeout.tv_sec = 2;
  timeout.tv_usec = 0;

  fd_set set;
  FD_ZERO(&set);
  FD_SET(sock, &set);

  if (!set_nonblock(sock, 1)) {
    goto ERROR_OUT;
  }

  connect(sock, addr->ai_addr, addr->ai_addrlen);

#if _WIN32
  if (WSAGetLastError() != WSAEWOULDBLOCK)
    goto ERROR_OUT;
#else
  if (errno != EAGAIN && errno != EWOULDBLOCK && errno != EINPROGRESS) {
    WSAErrno();
    elog("connect(): %s", strerror(errno));
    goto ERROR_OUT;
  }
#endif

  if (select(sock + 1, NULL, &set, NULL, &timeout) <= 0) {
    WSAErrno();
    elog("connect timeout/failed: %s", strerror(errno));
    goto ERROR_OUT;
  }

  if (!set_nonblock(sock, 0)) {
  ERROR_OUT:
    net_close(sock);
    return INVALID_SOCKET;
  }

  return sock;
}

socket_t net_connect(const char *host, uint16_t port) {
  dlog("connect: %s port %d", host, port);

  struct addrinfo hints = {0}, *addr = 0, *addrs = 0;
  hints.ai_family = AF_UNSPEC;
  hints.ai_socktype = SOCK_STREAM;
  hints.ai_protocol = IPPROTO_TCP;

  if (getaddrinfo(host, NULL, &hints, &addrs) != 0) {
    WSAErrno();
    elog("getaddrinfo failed (%d): %s", errno, strerror(errno));
    return INVALID_SOCKET;
  }

  addr = addrs;
  do {
    socket_t sock = net_connect(addr, port);
    if (sock != INVALID_SOCKET) {
      int len = 65536 * 4;
      setsockopt(sock, SOL_SOCKET, SO_RCVBUF, (char *)&len, sizeof(int));
      set_recv_timeout(sock, 5);
      return sock;
    }
  } while ((addr = addr->ai_next) != NULL);

  freeaddrinfo(addrs);
  return INVALID_SOCKET;
}

int net_recv(socket_t sock, void *buf, size_t len) {
#if _WIN32
  return recv(sock, (char *)buf, len, 0);
#else
  return recv(sock, buf, len, 0);
#endif
}

int net_recv_peek(socket_t sock) {
  char buf[4];
  return recv(sock, buf, 1, MSG_PEEK);
}

int net_recv_all(socket_t sock, void *buf, size_t len) {
#if _WIN32
  return recv(sock, (char *)buf, len, MSG_WAITALL);
#else
  return recv(sock, buf, len, MSG_WAITALL);
#endif
}

int net_send(socket_t sock, const void *buf, size_t len) {
#if _WIN32
  return send(sock, (const char *)buf, len, 0);
#else
  return send(sock, buf, len, 0);
#endif
}

int net_send_all(socket_t sock, const void *buf, size_t len) {
  int w = 0;
  char *ptr = (char *)buf;
  while (len > 0) {
    w = send(sock, ptr, len, 0);
    if (w <= 0) {
      return -1;
    }
    len -= w;
    ptr += w;
  }
  return 1;
}

bool net_close(socket_t sock) {
  shutdown(sock, SHUT_RDWR);
#ifdef _WIN32
  return !closesocket(sock);
#else
  return !close(sock);
#endif
}

bool net_init(void) {
#ifdef _WIN32
  WSADATA wsa;
  int res = WSAStartup(MAKEWORD(2, 2), &wsa) < 0;
  if (res < 0) {
    elog("WSAStartup failed with error %d", res);
    return false;
  }
#endif
  return true;
}

void net_cleanup(void) {
#ifdef _WIN32
  WSACleanup();
#endif
}
