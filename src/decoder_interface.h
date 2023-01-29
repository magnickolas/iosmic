#pragma once

#include <cstdint>
#include <mutex>
#include <obs/media-io/media-io-defs.h>
#include <vector>

#define xlog(log_level, format, ...)                                           \
  blog(log_level, "[DroidCamOBS] " format, ##__VA_ARGS__)

#define ilog(format, ...) xlog(LOG_INFO, format, ##__VA_ARGS__)
#define elog(format, ...) xlog(LOG_WARNING, format, ##__VA_ARGS__)

template <typename T> struct Queue {
  std::mutex items_lock;
  std::vector<T> items;

  void add_item(T item) {
    items_lock.lock();
    items.push_back(item);
    items_lock.unlock();
  }

  T next_item(void) {
    T item{};
    if (items.size()) {
      items_lock.lock();
      item = items.front();
      items.erase(items.begin());
      items_lock.unlock();
    }
    return item;
  }
};

struct DataPacket {
  uint8_t *data;
  size_t size;
  size_t used;
  uint64_t pts;

  DataPacket(size_t new_size) {
    size = 0;
    data = 0;
    resize(new_size);
  }

  ~DataPacket(void) {
    if (data)
      bfree(data);
  }

  void resize(size_t new_size) {
    if (size < new_size) {
      data = (uint8_t *)brealloc(data, new_size);
      size = new_size;
    }
  }
};

struct Decoder {
  Queue<DataPacket *> receiveQueue;
  Queue<DataPacket *> decodeQueue;
  size_t alloc_count;
  volatile bool ready;
  volatile bool failed;

  Decoder(void) {
    alloc_count = 0;
    ready = false;
    failed = false;
  }

  virtual ~Decoder(void) {
    DataPacket *packet;
    while ((packet = receiveQueue.next_item()) != NULL) {
      delete packet;
      alloc_count--;
    }
    while ((packet = decodeQueue.next_item()) != NULL) {
      delete packet;
      alloc_count--;
    }
    ilog("~decoder alloc_count=%lu", alloc_count);
  }

  inline DataPacket *pull_ready_packet(void) { return decodeQueue.next_item(); }

  DataPacket *pull_empty_packet(size_t size) {
    DataPacket *packet = receiveQueue.next_item();
    if (!packet) {
      packet = new DataPacket(size);
      ilog("@decoder alloc: size=%ld", size);
      alloc_count++;
    } else {
      packet->resize(size);
    }
    packet->used = 0;
    return packet;
  }

  inline void push_empty_packet(DataPacket *packet) {
    receiveQueue.add_item(packet);
  }

  virtual void push_ready_packet(DataPacket *) = 0;
  virtual bool decode_audio(DataPacket *, bool *got_output) = 0;
};
