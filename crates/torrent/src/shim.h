// libtorrent 的会话包一层给 Rust 用（声明见 src/lib.rs 的 ffi）。种子的编号在加入时定下：有 v1 信息哈希时为它的十六进制
// 串，否则为截到 20 字节的 v2 的；取到元数据后知道了另一种哈希也不变。会话对象可在多个线程上同时调用。
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
	rust::String add_resume(rust::Slice<std::uint8_t const> resume, rust::Str id, rust::Str save_path) const;
	void start(rust::Str id) const;
	void flush(rust::Str id) const;
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
	// 加入一个种子并登记，编号为 `id`
	rust::String add(lt::add_torrent_params params, std::string const& id, rust::Str save_path) const;
	lt::torrent_handle handle(rust::Str id) const;
	// 种子 `id` 的移除有了一个结果：`files_settled` 为删文件的结果（删完或删失败），否则为移除的事件
	void removed(std::string const& id, bool files_settled) const;

	// 一个还没移除完的种子：移除的事件来了没有；要删文件时删的结果来了没有（libtorrent 先发删的结果，后发移除的事件）
	struct Removing {
		bool deleting = false;
		bool removed = false;
		bool files_settled = false;
	};
	// 事件里的种子的编号；不是经这个会话加入的时抛出（加入失败的事件不来查）
	std::string id_of(lt::torrent_handle const& handle) const;

	mutable lt::session m_session;
	// 编号到句柄：移除即去掉，之后对这个编号的操作报「没有这个种子」
	mutable std::map<std::string, lt::torrent_handle> m_handles;
	// 句柄到编号：句柄按 libtorrent 的 owner_before 比较，种子移除、句柄失效后照样查得到。移除了的也不去掉：移除之后
	// 还会有它的事件（移除、删文件的结果，移除前要的续传数据的结果）。简化：句柄里的弱引用让 libtorrent 的整个种子
	// 对象（约 1.7 KB）留到会话结束，每加入并移除一个种子多占这么多；一个会话里加删上万个时改为在最后一个事件之后去掉
	mutable std::map<lt::torrent_handle, std::string> m_ids;
	// 调了 remove、还没移除完的种子的编号与它移除到了哪一步：移除完（移除的事件，删文件时还要删的结果）之前不许再加
	// 同一个种子，否则旧的那个还没发出的事件会带着同一个编号。事件队列满了丢了这几个事件时，这个会话里再也加不进它
	mutable std::map<std::string, Removing> m_removing;
	// 守着上面两张表；加入时整个过程持有，取事件时查编号不会早于登记
	mutable std::mutex m_mutex;
	// pop_alerts 拿到的事件在下一次 pop_alerts 之前有效：转成自有的数据之前不让别的线程再取
	mutable std::mutex m_alerts_mutex;
};

std::unique_ptr<Session> new_session(SessionConfig const& config);

// 为 `path`（文件或目录）建种子文件，分块大小 `piece_size` 字节（0 为按总大小自动选）；返回 bencode 写出的种子文件。
rust::Vec<std::uint8_t> create_torrent(rust::Str path, std::int32_t piece_size);

} // namespace hsdlx
