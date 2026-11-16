// libtorrent 的会话包一层给 Rust 用（声明见 src/lib.rs 的 ffi）。种子的编号在加入时定下：有 v1 信息哈希时为它的十六进制
// 串，否则为截到 20 字节的 v2 的；取到元数据后知道了另一种哈希也不变。会话对象可在多个线程上同时调用。
#pragma once

#include <cstdint>
#include <map>
#include <memory>
#include <mutex>
#include <set>
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
struct AddOptions;
struct Described;
struct NetworkStatus;
struct TrackerState;
enum class Removal : std::uint8_t;

class Session {
public:
	explicit Session(SessionConfig const& config);

	rust::String add_magnet(rust::Str uri, rust::Str save_path) const;
	rust::String add_resume(rust::Slice<std::uint8_t const> resume, rust::Str id, rust::Str save_path,
		AddOptions const& options) const;
	bool removing(rust::Str id) const;
	void start(rust::Str id) const;
	void flush(rust::Str id) const;
	rust::Vec<std::uint8_t> session_state() const;
	Status status(rust::Str id) const;
	void connect_peer(rust::Str id, rust::Str ip, std::uint16_t port) const;
	void pause(rust::Str id) const;
	void resume(rust::Str id) const;
	void remove(rust::Str id, Removal removal) const;
	void save_resume_data(rust::Str id) const;
	std::uint16_t listen_port() const;
	void set_upload_limit(std::int32_t bytes_per_second) const;
	NetworkStatus network() const;
	void request_stats() const;
	rust::Vec<TrackerState> trackers(rust::Str id) const;
	std::uint32_t add_trackers(rust::Str id, rust::Slice<rust::String const> urls) const;
	rust::Vec<Alert> pop_alerts(std::uint32_t wait_ms) const;

private:
	// 加入一个种子并登记，编号为 `id`
	rust::String add(lt::add_torrent_params params, std::string const& id, rust::Str save_path) const;
	lt::torrent_handle handle(rust::Str id) const;
	// 种子 `id` 有一个 flush 的请求在等写完的事件时取走它，返回 true
	bool flush_requested(std::string const& id) const;
	// 种子 `id` 的移除有了一个结果：`files_settled` 为删文件的结果（删完或删失败），否则为移除的事件
	void removed(std::string const& id, bool files_settled) const;

	// 一个还没移除完的种子：移除的事件来了没有；要删文件时删的结果来了没有（libtorrent 先发删的结果，后发移除的事件）
	struct Removing {
		lt::torrent_handle handle;
		// 登记之后丢过事件：移除的结果可能丢了，要按种子对象还在不在判断
		bool lost = false;
		bool deleting = false;
		bool removed = false;
		bool files_settled = false;
	};
	// 种子 `id` 还在移除中：登记了移除、事件还没都来。登记之后丢过事件的，种子对象已不在了即算移除完（移除与删文件都
	// 做完时它就不在了，删文件的磁盘任务持有它直到删完）；没丢过的只认事件，libtorrent 发出移除的事件后就放掉种子对象，
	// 那时事件可能还没取走。调用方持有 m_mutex
	bool still_removing(std::string const& id) const;
	// 丢了事件：在移除中的都记下
	void mark_lost() const;
	// 从种子 `id` 的续传数据里去掉 add_trackers 补的 tracker
	void drop_added_trackers(std::string const& id, lt::add_torrent_params& params) const;
	// 事件里的种子的编号；不是经这个会话加入的时抛出（加入失败的事件不来查）
	std::string id_of(lt::torrent_handle const& handle) const;
	// `handle` 是编号 `id` 现在登记的那一份（在会话里，或移除中还没移除完）：同一个种子移除后再加入时，旧的那份的事件
	// 不算
	bool current(std::string const& id, lt::torrent_handle const& handle) const;

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
	// 调了 flush、还没报来写完的次数，按编号：写完的事件按它转出，其余的（种子下完时 libtorrent 自己发的）不转。下完时
	// 自发的那条若在请求之后才到，会被当作请求的结果：它写盘时已下的就是全部要下的，与请求的保证相同
	mutable std::map<std::string, int> m_flush_requests;
	// add_trackers 给各种子补的 tracker，按编号：写续传数据时去掉，不随种子存下；再加入同一个种子时清空
	mutable std::map<std::string, std::set<std::string>> m_added_trackers;
	// 守着上面几张表；加入时整个过程持有，取事件时查编号不会早于登记
	mutable std::mutex m_mutex;
	// pop_alerts 拿到的事件在下一次 pop_alerts 之前有效：转成自有的数据之前不让别的线程再取
	mutable std::mutex m_alerts_mutex;

	// 取事件时记下的网络状况（见 network()）
	struct Network {
		std::string listen_error;
		std::uint16_t upnp_port = 0;
		std::string upnp_error;
		std::uint16_t natpmp_port = 0;
		std::string natpmp_error;
		std::int64_t dht_nodes = -1;
		bool incoming = false;
	};
	mutable Network m_network;
	// 守着 m_network
	mutable std::mutex m_network_mutex;
};

std::unique_ptr<Session> new_session(SessionConfig const& config);

// 种子文件（bencode 写的）转成续传数据（还没下过的）。
rust::Vec<std::uint8_t> resume_from_torrent_file(rust::Slice<std::uint8_t const> torrent);
// 续传数据里的种子信息；没有种子信息时抛出。
Described describe(rust::Slice<std::uint8_t const> resume);
// 新建任务前定下内容放哪、下哪些：把续传数据里的种子根名（单文件的文件名、多文件的顶层目录名）改成 `root`，只下序号在
// `wanted` 里的文件，去掉只上传、暂停、做种模式的标志；返回改后的续传数据。
rust::Vec<std::uint8_t> place(rust::Slice<std::uint8_t const> resume, rust::Str root, rust::Slice<std::int32_t const> wanted);

// 为 `path`（文件或目录）建种子文件，分块大小 `piece_size` 字节（0 为按总大小自动选），`v1_only` 时只有 v1 的信息哈希、
// 文件不按块对齐；返回 bencode 写出的种子文件。
rust::Vec<std::uint8_t> create_torrent(rust::Str path, std::int32_t piece_size, bool v1_only);

} // namespace hsdlx
