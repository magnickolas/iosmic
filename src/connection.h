#pragma once

#define INVALID_SOCKET -1
typedef int SOCKET;
typedef long int SOCKET_PTR;

SOCKET Connect(const char *ip, int port, const char **errormsg);
void connection_cleanup();
void disconnect(SOCKET s);

SOCKET accept_connection(int port);
int Send(const char *buffer, int bytes, SOCKET s);
int Recv(const char *buffer, int bytes, SOCKET s);
int RecvAll(const char *buffer, int bytes, SOCKET s);
int RecvNonBlock(char *buffer, int bytes, SOCKET s);
