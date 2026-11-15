#include "hs-dlx-torrent/src/shim.h"
#include "hs-dlx-torrent/src/lib.rs.h"

#include <algorithm>
#include <chrono>
#include <filesystem>
#include <stdexcept>

#include <libtorrent/add_torrent_params.hpp>
#include <libtorrent/alert_types.hpp>
#include <libtorrent/create_torrent.hpp>
#include <libtorrent/fingerprint.hpp>
#include <libtorrent/hex.hpp>
#include <libtorrent/load_torrent.hpp>
#include <libtorrent/magnet_uri.hpp>
#include <libtorrent/settings_pack.hpp>
#include <libtorrent/torrent_status.hpp>
#include <libtorrent/write_resume_data.hpp>

namespace hsdlx {

namespace {

std::string string_of(rust::Str text) { return std::string(text.data(), text.size()); }

// 种子的指认：信息哈希的十六进制串
std::string id_of(lt::torrent_handle const& handle) {
	return lt::aux::to_hex(handle.info_hashes().get_best());
}

lt::settings_pack settings_of(SessionConfig const& config) {
	lt::settings_pack pack;
	pack.set_str(lt::settings_pack::listen_interfaces, string_of(config.listen_interfaces));
	pack.set_bool(lt::settings_pack::enable_dht, config.dht);
	pack.set_bool(lt::settings_pack::enable_upnp, config.upnp);
	pack.set_bool(lt::settings_pack::enable_natpmp, config.natpmp);
	pack.set_bool(lt::settings_pack::enable_lsd, config.lsd);
	pack.set_str(lt::settings_pack::user_agent, string_of(config.user_agent));
	pack.set_str(lt::settings_pack::peer_fingerprint, string_of(config.peer_fingerprint));
	// 协议加密：两个方向都允许，优先加密，对方只会明文时照样连
	pack.set_int(lt::settings_pack::out_enc_policy, lt::settings_pack::pe_enabled);
	pack.set_int(lt::settings_pack::in_enc_policy, lt::settings_pack::pe_enabled);
	pack.set_int(lt::settings_pack::allowed_enc_level, lt::settings_pack::pe_both);
	pack.set_bool(lt::settings_pack::prefer_rc4, true);
	// 只要出错、状态、存储与端口映射的事件：tracker 与节点的事件量大，诊断时另开
	pack.set_int(lt::settings_pack::alert_mask,
		static_cast<int>(static_cast<std::uint32_t>(lt::alert_category::error
			| lt::alert_category::status
			| lt::alert_category::storage
			| lt::alert_category::port_mapping)));
	return pack;
}

Alert alert_of(AlertKind kind, lt::alert const& alert, std::string torrent) {
	Alert out;
	out.kind = kind;
	out.torrent = rust::String(torrent);
	out.message = rust::String(alert.message());
	return out;
}

} // namespace

Session::Session(SessionConfig const& config)
	: m_session(lt::session_params(settings_of(config)))
{}

std::unique_ptr<Session> new_session(SessionConfig const& config) {
	return std::make_unique<Session>(config);
}

rust::String Session::remember(lt::torrent_handle const& handle) const {
	std::string id = id_of(handle);
	std::lock_guard<std::mutex> lock(m_handles_mutex);
	m_handles[id] = handle;
	return rust::String(id);
}

lt::torrent_handle Session::handle(rust::Str id) const {
	std::lock_guard<std::mutex> lock(m_handles_mutex);
	auto found = m_handles.find(string_of(id));
	if (found == m_handles.end()) throw std::invalid_argument("没有这个种子：" + string_of(id));
	return found->second;
}

rust::String Session::add_magnet(rust::Str uri, rust::Str save_path) const {
	lt::add_torrent_params params = lt::parse_magnet_uri(string_of(uri));
	params.save_path = string_of(save_path);
	return remember(m_session.add_torrent(std::move(params)));
}

rust::String Session::add_torrent(rust::Slice<std::uint8_t const> torrent, rust::Str save_path) const {
	lt::add_torrent_params params = lt::load_torrent_buffer(
		lt::span<char const>(reinterpret_cast<char const*>(torrent.data()), static_cast<std::ptrdiff_t>(torrent.size())));
	params.save_path = string_of(save_path);
	return remember(m_session.add_torrent(std::move(params)));
}

Status Session::status(rust::Str id) const {
	lt::torrent_handle h = handle(id);
	lt::torrent_status s = h.status(lt::torrent_handle::query_pieces | lt::torrent_handle::query_name);
	Status out;
	out.state = static_cast<std::uint8_t>(s.state);
	out.has_metadata = s.has_metadata;
	out.paused = static_cast<bool>(s.flags & lt::torrent_flags::paused);
	out.name = rust::String(s.name);
	out.total_wanted = s.total_wanted;
	out.total_wanted_done = s.total_wanted_done;
	out.download_rate = s.download_payload_rate;
	out.upload_rate = s.upload_payload_rate;
	out.peers = s.num_peers;
	out.seeds = s.num_seeds;
	out.error = rust::String(s.errc ? s.errc.message() : std::string());
	// 能凑齐的块：自己已有的，或在线的节点里至少有一个有的
	std::vector<int> availability;
	if (s.has_metadata) h.piece_availability(availability);
	out.pieces = static_cast<std::int32_t>(s.pieces.size());
	std::int32_t reachable = 0;
	for (int i = 0; i < s.pieces.size(); ++i) {
		lt::piece_index_t const piece(i);
		bool const available = i < static_cast<int>(availability.size()) && availability[std::size_t(i)] > 0;
		if (s.pieces.get_bit(piece) || available) ++reachable;
	}
	out.reachable_pieces = reachable;
	return out;
}

void Session::connect_peer(rust::Str id, rust::Str ip, std::uint16_t port) const {
	boost::system::error_code ec;
	auto address = boost::asio::ip::make_address(string_of(ip), ec);
	if (ec) throw std::invalid_argument("不是 IP 地址：" + string_of(ip));
	handle(id).connect_peer(lt::tcp::endpoint(address, port));
}

void Session::pause(rust::Str id) const { handle(id).pause(lt::torrent_handle::graceful_pause); }

void Session::resume(rust::Str id) const { handle(id).resume(); }

void Session::remove(rust::Str id, bool delete_files) const {
	lt::torrent_handle h = handle(id);
	m_session.remove_torrent(h, delete_files ? lt::session::delete_files : lt::remove_flags_t{});
	std::lock_guard<std::mutex> lock(m_handles_mutex);
	m_handles.erase(string_of(id));
}

void Session::save_resume_data(rust::Str id) const {
	handle(id).save_resume_data(lt::torrent_handle::save_info_dict);
}

std::uint16_t Session::listen_port() const { return m_session.listen_port(); }

rust::Vec<Alert> Session::pop_alerts(std::uint32_t wait_ms) const {
	rust::Vec<Alert> out;
	std::lock_guard<std::mutex> lock(m_alerts_mutex);
	m_session.wait_for_alert(std::chrono::milliseconds(wait_ms));
	std::vector<lt::alert*> alerts;
	m_session.pop_alerts(&alerts);
	for (lt::alert* a : alerts) {
		if (auto* added = lt::alert_cast<lt::add_torrent_alert>(a)) {
			if (added->error) {
				out.push_back(alert_of(AlertKind::AddFailed, *a, std::string()));
			} else {
				out.push_back(alert_of(AlertKind::Added, *a, id_of(added->handle)));
			}
		} else if (auto* t = lt::alert_cast<lt::metadata_received_alert>(a)) {
			out.push_back(alert_of(AlertKind::Metadata, *a, id_of(t->handle)));
		} else if (auto* t = lt::alert_cast<lt::torrent_finished_alert>(a)) {
			out.push_back(alert_of(AlertKind::Finished, *a, id_of(t->handle)));
		} else if (auto* t = lt::alert_cast<lt::save_resume_data_alert>(a)) {
			Alert alert = alert_of(AlertKind::ResumeData, *a, id_of(t->handle));
			std::vector<char> data = lt::write_resume_data_buf(t->params);
			for (char c : data) alert.data.push_back(static_cast<std::uint8_t>(c));
			out.push_back(std::move(alert));
		} else if (auto* t = lt::alert_cast<lt::save_resume_data_failed_alert>(a)) {
			out.push_back(alert_of(AlertKind::ResumeDataFailed, *a, id_of(t->handle)));
		} else if (auto* t = lt::alert_cast<lt::torrent_error_alert>(a)) {
			out.push_back(alert_of(AlertKind::TorrentError, *a, id_of(t->handle)));
		} else if (auto* t = lt::alert_cast<lt::file_error_alert>(a)) {
			out.push_back(alert_of(AlertKind::FileError, *a, id_of(t->handle)));
		} else if (auto* t = lt::alert_cast<lt::torrent_removed_alert>(a)) {
			out.push_back(alert_of(AlertKind::Removed, *a, lt::aux::to_hex(t->info_hashes.get_best())));
		} else if (lt::alert_cast<lt::listen_succeeded_alert>(a)) {
			out.push_back(alert_of(AlertKind::ListenSucceeded, *a, std::string()));
		} else if (lt::alert_cast<lt::listen_failed_alert>(a)) {
			out.push_back(alert_of(AlertKind::ListenFailed, *a, std::string()));
		} else if (lt::alert_cast<lt::portmap_alert>(a)) {
			out.push_back(alert_of(AlertKind::PortMapped, *a, std::string()));
		} else if (lt::alert_cast<lt::portmap_error_alert>(a)) {
			out.push_back(alert_of(AlertKind::PortMapFailed, *a, std::string()));
		}
	}
	return out;
}

rust::Vec<std::uint8_t> create_torrent(rust::Str path, std::int32_t piece_size) {
	std::filesystem::path const full = std::filesystem::absolute(string_of(path));
	lt::create_torrent torrent(lt::list_files(full.string()), piece_size);
	lt::set_piece_hashes(torrent, full.parent_path().string());
	std::vector<char> buffer = torrent.generate_buf();
	rust::Vec<std::uint8_t> out;
	out.reserve(buffer.size());
	for (char c : buffer) out.push_back(static_cast<std::uint8_t>(c));
	return out;
}

} // namespace hsdlx
