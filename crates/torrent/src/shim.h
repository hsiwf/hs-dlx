// libtorrent 的会话包一层给 Rust 用（声明见 src/lib.rs 的 ffi）。种子用信息哈希的十六进制串（v1 的 SHA-1，只有 v2 时为
// 截到 20 字节的 SHA-256）指认；会话对象可在多个线程上同时调用。
#pragma once

#include <cstdint>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <vector>

#include <libtorrent/session.hpp>
#include <libtorrent/torrent_handle.hpp>

#include "rust/cxx.h"

namespace hsdlx {

struct SessionConfig;
struct Status;
struct Alert;
struct FileEntry;

class Session {
public:
	explicit Session(SessionConfig const& config);

	rust::String add_magnet(rust::Str uri, rust::Str save_path) const;
	rust::String add_torrent(rust::Slice<std::uint8_t const> torrent, rust::Str save_path) const;
	rust::String add_resume(rust::Slice<std::uint8_t const> resume, rust::Str save_path) const;
	rust::Vec<FileEntry> files(rust::Str id) const;
	void select_files(rust::Str id, rust::Slice<std::int32_t const> wanted) const;
	rust::Vec<std::uint8_t> session_state() const;
	Status status(rust::Str id) const;
	void connect_peer(rust::Str id, rust::Str ip, std::uint16_t port) const;
	void pause(rust::Str id) const;
	void resume(rust::Str id) const;
	void remove(rust::Str id, bool delete_files) const;
	void save_resume_data(rust::Str id) const;
	std::uint16_t listen_port() const;
	rust::Vec<Alert> pop_alerts(std::uint32_t wait_ms) const;

private:
	// 加进来的种子；会话自己按信息哈希管，这里只为不必每次把十六进制串解回哈希
	lt::torrent_handle handle(rust::Str id) const;
	rust::String remember(lt::torrent_handle const& handle) const;

	mutable lt::session m_session;
	mutable std::mutex m_handles_mutex;
	mutable std::map<std::string, lt::torrent_handle> m_handles;
	// pop_alerts 拿到的事件在下一次 pop_alerts 之前有效：转成自有的数据之前不让别的线程再取
	mutable std::mutex m_alerts_mutex;
};

std::unique_ptr<Session> new_session(SessionConfig const& config);

// 为 `path`（文件或目录）建种子文件，分块大小 `piece_size` 字节（0 为按总大小自动选）；返回 bencode 写出的种子文件。
rust::Vec<std::uint8_t> create_torrent(rust::Str path, std::int32_t piece_size);

} // namespace hsdlx
