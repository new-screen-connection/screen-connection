//! 软 KVM（P1-2）：一台显示器 + 两台电脑，一键切输入源 / 音频 / 剪贴板。
//!
//! 设计要点（对齐开发提示词包 §十）：
//! - 配对：发起端生成 6 位配对码 + 随机 32 字节 PSK；另一端输码完成绑定。
//!   PSK 通过 TCP 明文帧在配对窗口内下发——短窗口 + 6 位码是凭证，
//!   第一阶段「够用」（不做证书体系）。
//! - 发现：UDP 广播 47821，报文只含设备名 + 配对状态，不含任何硬件信息。
//! - 协议：TCP 47822；已配对消息一律 AES-256-GCM 加密（PSK 派生），帧头带
//!   4 字节对端指纹前缀供选钥；未配对设备的任何指令一律丢弃并记日志（不含内容）。
//! - 剪贴板：纯文本上限 64KB，超限直接拒绝；图片/文件明确不支持。
//! - 键鼠转发不在本阶段（Synergy 的领域）。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::{aead::Aead, aead::KeyInit, Aes256Gcm, Key};

/// UDP 发现端口（固定）
const UDP_PORT: u16 = 47821;
/// TCP 协议端口（固定）
const TCP_PORT: u16 = 47822;
/// 广播报文应用标识（别的软件撞端口时直接忽略）
const APP_TAG: &str = "screenguard-kvm-1";
const FRAME_MAGIC: u16 = 0x5347; // "SG"
const FRAME_VER: u8 = 1;
/// 帧标志位：明文（仅配对握手） / PSK 加密
const FLAG_PLAIN: u8 = 0;
const FLAG_AESGCM: u8 = 1;
/// 剪贴板文本上限（64KB，UTF-8 字节）
pub const CLIP_LIMIT: usize = 64 * 1024;
/// 单帧上限
const MAX_FRAME: usize = 200 * 1024;
/// 发现条目新鲜期
const FRESH_SECS: u64 = 15;
/// 配对码有效期
const PENDING_TTL: Duration = Duration::from_secs(5 * 60);
/// 对端 ACK 超时
const ACK_TIMEOUT: Duration = Duration::from_secs(5);

// ---------- 契约 ----------

/// 已配对的对端（序列化给前端；psk 永不出后端）
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct KvmPeer {
    pub id: String,
    pub name: String,
    /// 上次直连地址 ip:port
    pub addr: String,
    /// sha256(psk) 前 8 字节 hex，供两端人工比对
    pub fingerprint: String,
    /// 本机在这块共享屏上的输入口（对端把屏切回来时用的口），如 "hdmi1"
    #[serde(default = "default_source")]
    pub my_source: String,
    pub paired_at: String,
}

fn default_source() -> String {
    "hdmi1".to_string()
}

/// 本地存储记录（psk 只落本机文件）
#[derive(Serialize, Deserialize, Clone, Debug)]
struct PeerRecord {
    peer: KvmPeer,
    /// 32 字节 PSK 的 hex（64 字符）
    psk: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct KvmDevice {
    pub name: String,
    pub ip: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct KvmDevices {
    pub me: String,
    pub peers: Vec<KvmPeer>,
    pub discovered: Vec<KvmDevice>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct KvmSwitchReport {
    /// 对端是否确认（false 时输入源仍会切，但界面要提示未确认）
    pub ack_ok: bool,
    pub ack_detail: String,
    /// 本机 DDC 是否成功把屏切到对端口
    pub input_ok: bool,
    pub input_msg: String,
    /// 本机音频切回本机扬声器的结果
    pub audio_msg: String,
    /// 剪贴板发送结果
    pub clip_msg: String,
}

// ---------- 协议消息 ----------

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "t")]
enum KvmMsg {
    /// 配对请求（明文帧；配对码即凭证）
    PairReq { code: String, name: String },
    /// 配对接受：下发 PSK（明文帧，短窗口）
    PairAcc { psk: String, name: String },
    PairRej { reason: String },
    /// 切换指令（加密帧）
    Switch { source: String, clipboard: Option<String>, from: String },
    Ack { ok: bool, detail: Option<String> },
}

// ---------- 全局状态 ----------

static PEERS: OnceLock<Mutex<Vec<PeerRecord>>> = OnceLock::new();
/// 等待中的配对：(配对码, PSK, 过期时刻)。同一次只挂一个发起。
static PENDING: Mutex<Option<(String, [u8; 32], Instant)>> = Mutex::new(None);
/// 发现表：ip → (设备名, 最后看见时刻)
static DISCOVERED: OnceLock<Mutex<HashMap<String, (String, Instant)>>> = OnceLock::new();
static SERVER_STARTED: AtomicBool = AtomicBool::new(false);
static APP: OnceLock<tauri::AppHandle> = OnceLock::new();

fn peers_slot() -> &'static Mutex<Vec<PeerRecord>> {
    PEERS.get_or_init(|| Mutex::new(load_records()))
}

fn discovered_slot() -> &'static Mutex<HashMap<String, (String, Instant)>> {
    DISCOVERED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 事件：kvm-status { stage: progress|done|partial|failed|info, message }
fn emit_status(stage: &str, message: &str) {
    if let Some(app) = APP.get() {
        use tauri::Emitter;
        let _ = app.emit(
            "kvm-status",
            serde_json::json!({ "stage": stage, "message": message }),
        );
    }
}

pub fn set_app_handle(app: tauri::AppHandle) {
    let _ = APP.set(app);
}

// ---------- 存储 ----------

fn peers_file() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        PathBuf::from(std::env::var("APPDATA").unwrap_or_default())
            .join("Screenguard")
            .join("peers.json")
    }
    #[cfg(target_os = "macos")]
    {
        // macOS 的 app_support_dir 可能失败（目录创建失败等），回退临时目录
        crate::platform::app_support_dir()
            .map(|d| d.join("peers.json"))
            .unwrap_or_else(|_| std::env::temp_dir().join("screenguard-peers.json"))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::env::temp_dir().join("screenguard-peers.json")
    }
}

fn load_records() -> Vec<PeerRecord> {
    let path = peers_file();
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    match serde_json::from_str::<Vec<PeerRecord>>(&raw) {
        Ok(v) => v,
        Err(e) => {
            // 损坏：rename 保底，绝不覆盖用户数据
            eprintln!("[screenguard] peers.json 解析失败（{}），已备份为 .bak", e);
            let bak = path.with_extension("json.bak");
            std::fs::rename(&path, bak).ok();
            Vec::new()
        }
    }
}

fn save_records(list: &[PeerRecord]) -> Result<(), String> {
    let path = peers_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建配置目录失败：{}", e))?;
    }
    let json = serde_json::to_string_pretty(list).map_err(|e| format!("序列化失败：{}", e))?;
    std::fs::write(&path, json).map_err(|e| format!("写入 peers.json 失败：{}", e))
}

fn with_peers<T>(f: impl FnOnce(&mut Vec<PeerRecord>) -> T) -> Result<T, String> {
    let mut g = peers_slot().lock().map_err(|_| "peers 锁中毒".to_string())?;
    let out = f(&mut g);
    save_records(&g)?;
    Ok(out)
}

// ---------- 随机材料 ----------

fn random_bytes(n: usize) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; n];
    getrandom::getrandom(&mut buf).map_err(|e| format!("随机数生成失败：{}", e))?;
    Ok(buf)
}

fn fingerprint_of(psk: &[u8]) -> String {
    let h = Sha256::digest(psk);
    hex::encode(&h[..8])
}

fn sanitize_name(raw: &str) -> String {
    let t: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect();
    t.trim().to_string()
}

// ---------- 帧编解码 ----------
// 布局：[u32 总长 BE][magic u16 BE][ver u8][flag u8][payload]
// flag=1 时 payload = [nonce 12B][pid 8B（指纹前 8 个 hex 字符）][密文+tag]

fn send_frame(stream: &mut TcpStream, msg: &KvmMsg, psk: Option<&[u8]>) -> Result<(), String> {
    let body = serde_json::to_vec(msg).map_err(|e| format!("消息序列化失败：{}", e))?;
    let (flag, payload) = match psk {
        None => (FLAG_PLAIN, body),
        Some(k) => {
            let mut nonce = [0u8; 12];
            getrandom::getrandom(&mut nonce).map_err(|e| format!("随机数生成失败：{}", e))?;
            let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(k));
            let ct = cipher
                .encrypt(GenericArray::from_slice(&nonce), body.as_ref())
                .map_err(|_| "帧加密失败".to_string())?;
            let pid = fingerprint_of(k);
            let pid8 = &pid.as_bytes()[..8.min(pid.len())];
            let mut p = nonce.to_vec();
            p.extend_from_slice(pid8);
            p.extend_from_slice(&ct);
            (FLAG_AESGCM, p)
        }
    };
    if payload.len() > MAX_FRAME {
        return Err("帧超限".to_string());
    }
    let mut frame = Vec::with_capacity(payload.len() + 8);
    frame.extend_from_slice(&FRAME_MAGIC.to_be_bytes());
    frame.push(FRAME_VER);
    frame.push(flag);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&payload);
    let mut full = (frame.len() as u32).to_be_bytes().to_vec();
    full.extend_from_slice(&frame);
    stream.set_write_timeout(Some(Duration::from_secs(3))).ok();
    use std::io::Write;
    stream
        .write_all(&full)
        .map_err(|e| format!("发送失败：{}", e))
}

/// resolver：按帧内 pid（指纹前 8 hex 字符）取对应 PSK；客户端只和已知对端通信时也用它校验
fn recv_frame(
    stream: &mut TcpStream,
    resolver: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Result<(String, KvmMsg), String> {
    use std::io::Read;
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .map_err(|_| "连接中断".to_string())?;
    let total = u32::from_be_bytes(len_buf) as usize;
    if total > MAX_FRAME || total < 8 {
        return Err("帧长度非法".to_string());
    }
    let mut frame = vec![0u8; total];
    stream.read_exact(&mut frame).map_err(|_| "帧读取中断".to_string())?;
    if frame[0..2] != FRAME_MAGIC.to_be_bytes() {
        return Err("帧魔数不符（非本应用流量）".to_string());
    }
    if frame[2] != FRAME_VER {
        return Err(format!("协议版本不支持：{}", frame[2]));
    }
    let flag = frame[3];
    let plen = u32::from_be_bytes([frame[4], frame[5], frame[6], frame[7]]) as usize;
    if 8 + plen != total {
        return Err("帧长度字段不一致".to_string());
    }
    let payload = &frame[8..];
    let body: Vec<u8> = match flag {
        FLAG_PLAIN => payload.to_vec(),
        FLAG_AESGCM => {
            if payload.len() < 12 + 8 + 16 {
                return Err("加密帧过短".to_string());
            }
            let nonce = &payload[..12];
            let pid = String::from_utf8_lossy(&payload[12..20]).to_string();
            let ct = &payload[20..];
            let key = resolver(&pid).ok_or_else(|| {
                "未配对设备或未知指纹".to_string()
            })?;
            let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
            cipher
                .decrypt(GenericArray::from_slice(nonce), ct)
                .map_err(|_| "解密失败（密钥不匹配或帧损坏）".to_string())?
        }
        _ => return Err("帧标志非法".to_string()),
    };
    let msg: KvmMsg =
        serde_json::from_slice(&body).map_err(|_| "消息解析失败".to_string())?;
    let pid_out = if flag == FLAG_AESGCM {
        String::from_utf8_lossy(&payload[12..20]).to_string()
    } else {
        String::new()
    };
    Ok((pid_out, msg))
}

// ---------- 服务（TCP + UDP 发现） ----------

/// 启动发现与协议服务（幂等；setup 时调用一次）
pub fn ensure_started() {
    if SERVER_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(discovery_loop);
    std::thread::spawn(server_loop);
}

fn announce_payload() -> Vec<u8> {
    let peers_n = peers_slot().lock().map(|g| g.len()).unwrap_or(0);
    serde_json::json!({
        "app": APP_TAG,
        "name": crate::platform::kvm_device_name(),
        "peers": peers_n,
    })
    .to_string()
    .into_bytes()
}

fn discovery_loop() {
    let sock = match UdpSocket::bind(("0.0.0.0", UDP_PORT)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[screenguard] UDP 发现端口 {} 绑定失败：{}（发现功能不可用）", UDP_PORT, e);
            return;
        }
    };
    sock.set_broadcast(true).ok();
    sock.set_read_timeout(Some(Duration::from_secs(1))).ok();
    let mut last_announce = Instant::now() - Duration::from_secs(10);
    let mut buf = [0u8; 1024];
    loop {
        if last_announce.elapsed() >= Duration::from_secs(3) {
            let p = announce_payload();
            sock.send_to(&p, ("255.255.255.255", UDP_PORT)).ok();
            sock.send_to(&p, ("127.255.255.255", UDP_PORT)).ok();
            last_announce = Instant::now();
        }
        match sock.recv_from(&mut buf) {
            Ok((n, src)) => {
                let Ok(s) = std::str::from_utf8(&buf[..n]) else { continue };
                let Ok(v) = serde_json::from_str::<serde_json::Value>(s) else { continue };
                if v.get("app").and_then(|a| a.as_str()) != Some(APP_TAG) {
                    continue;
                }
                let name = sanitize_name(v.get("name").and_then(|n| n.as_str()).unwrap_or(""));
                if name.is_empty() || name == crate::platform::kvm_device_name() {
                    continue; // 自己的回声
                }
                let ip = src.ip().to_string();
                if let Ok(mut g) = discovered_slot().lock() {
                    g.insert(ip, (name, Instant::now()));
                }
            }
            Err(_) => continue, // 超时或暂时错误：下一轮继续
        }
    }
}

fn find_psk_by_pid(pid: &str) -> Option<Vec<u8>> {
    let g = peers_slot().lock().ok()?;
    g.iter()
        .find(|r| r.peer.fingerprint.starts_with(pid))
        .and_then(|r| hex::decode(&r.psk).ok())
}

fn server_loop() {
    let listener = match TcpListener::bind(("0.0.0.0", TCP_PORT)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[screenguard] TCP 协议端口 {} 绑定失败：{}（软 KVM 服务不可用）", TCP_PORT, e);
            return;
        }
    };
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let _ = std::thread::Builder::new()
                    .name("kvm-conn".into())
                    .spawn(move || handle_conn(s));
            }
            Err(e) => eprintln!("[screenguard] 接受连接失败：{}", e),
        }
    }
}

fn handle_conn(mut stream: TcpStream) {
    stream.set_read_timeout(Some(ACK_TIMEOUT)).ok();
    let peer_addr = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    let (pid, msg) = match recv_frame(&mut stream, &find_psk_by_pid) {
        Ok(v) => v,
        Err(e) => {
            // 未配对/垃圾流量：只记一句，不含任何内容
            eprintln!("[screenguard] 丢弃来自 {} 的无效流量（{}）", peer_addr, e);
            return;
        }
    };
    match msg {
        KvmMsg::PairReq { code, name } => {
            handle_pair_req(&mut stream, &code, &name, &peer_addr);
        }
        KvmMsg::Switch { source, clipboard, from } => {
            if pid.is_empty() {
                eprintln!("[screenguard] 丢弃来自 {} 的明文切换指令", peer_addr);
                return;
            }
            let psk = find_psk_by_pid(&pid);
            let (ok, detail) = handle_switch(&source, clipboard.as_deref());
            eprintln!(
                "[screenguard] 收到 {} 的切换指令（来源 {}）：{}",
                peer_addr,
                sanitize_name(&from),
                detail
            );
            if let Some(k) = psk {
                let _ = send_frame(&mut stream, &KvmMsg::Ack { ok, detail: Some(detail) }, Some(&k));
            }
        }
        other => {
            eprintln!("[screenguard] 丢弃来自 {} 的非配对消息（{}）", peer_addr, msg_kind(&other));
        }
    }
}

/// 配对请求（发起端的 TCP 服务处理）：码对上 → 下发 PSK，双方各存一条记录
fn handle_pair_req(stream: &mut TcpStream, code: &str, raw_name: &str, addr: &str) {
    let name = sanitize_name(raw_name);
    let pending = {
        let g = PENDING.lock().unwrap();
        match g.as_ref() {
            Some((c, psk, expires)) if Instant::now() < *expires && c == code => {
                Some((hex::encode(psk), name.clone()))
            }
            _ => None,
        }
    };
    match pending {
        Some((psk_hex, _)) => {
            let Ok(psk) = hex::decode(&psk_hex) else {
                let _ = send_frame(stream, &KvmMsg::PairRej { reason: "本机密钥损坏，请重新生成配对码".to_string() }, None);
                return;
            };
            let fp = fingerprint_of(&psk);
            let rec = PeerRecord {
                peer: KvmPeer {
                    id: hex::encode(random_bytes(8).unwrap_or_else(|_| vec![0; 8])),
                    name,
                    addr: format!("{}:{}", addr.rsplit_once(':').map(|x| x.0).unwrap_or(addr), TCP_PORT),
                    fingerprint: fp.clone(),
                    my_source: default_source(),
                    paired_at: crate::platform::now_str(),
                },
                psk: hex::encode(psk),
            };
            let saved = with_peers(|list| {
                // 同指纹 = 重复配对同一台：更新而不是堆叠
                if let Some(old) = list.iter_mut().find(|r| r.peer.fingerprint == fp) {
                    old.peer.name = rec.peer.name.clone();
                    old.peer.addr = rec.peer.addr.clone();
                    old.peer.paired_at = rec.peer.paired_at.clone();
                    old.psk = rec.psk.clone();
                } else {
                    list.push(rec);
                }
            });
            match saved {
                Ok(()) => {
                    let acc = KvmMsg::PairAcc {
                        psk: psk_hex,
                        name: crate::platform::kvm_device_name(),
                    };
                    if send_frame(stream, &acc, None).is_ok() {
                        emit_status("info", "有新设备完成配对");
                        let _ = PENDING.lock().unwrap().take();
                    }
                }
                Err(e) => {
                    let _ = send_frame(stream, &KvmMsg::PairRej { reason: e }, None);
                }
            }
        }
        None => {
            let reason = {
                let g = PENDING.lock().unwrap();
                match g.as_ref() {
                    Some((_, _, expires)) if Instant::now() >= *expires => {
                        "配对码已过期，请重新生成".to_string()
                    }
                    Some(_) => "配对码不正确".to_string(),
                    None => "本机没有正在等待的配对（先在发起端生成配对码）".to_string(),
                }
            };
            let _ = send_frame(stream, &KvmMsg::PairRej { reason }, None);
        }
    }
}

// ---------- 切换处理 ----------

/// 接管侧执行：切音频到显示器 + 写剪贴板。返回（整体是否成功，明细）
fn handle_switch(source: &str, clipboard: Option<&str>) -> (bool, String) {
    let mut notes: Vec<String> = Vec::new();
    let mut any_ok = false;
    let mut any_fail = false;

    // ① 音频切到「显示器自带音频」（Windows：form_factor==9 = HDMI/DP 音频）
    match try_audio_to_monitor() {
        Ok(msg) => {
            notes.push(msg);
            any_ok = true;
        }
        Err(e) => {
            notes.push(format!("音频：{}", e));
            any_fail = true;
        }
    }

    // ② 剪贴板（纯文本、64KB 上限，超限直接拒绝）
    match clipboard {
        Some(text) => {
            if text.len() > CLIP_LIMIT {
                notes.push("剪贴板：超过 64KB 上限，已拒绝".to_string());
                any_fail = true;
            } else if text.is_empty() {
                notes.push("剪贴板：对方剪贴板为空，跳过".to_string());
            } else {
                match crate::platform::kvm_clip_set(text) {
                    Ok(()) => {
                        notes.push("剪贴板：已写入".to_string());
                        any_ok = true;
                    }
                    Err(e) => {
                        notes.push(format!("剪贴板：写入失败（{}）", e));
                        any_fail = true;
                    }
                }
            }
        }
        None => notes.push("剪贴板：对方没有可发送的文本".to_string()),
    }

    // source 是对端口（对端告诉我们它要切到哪个口），本机在接管侧暂不执行 DDC——
    // 发起侧（当前握有画面）会执行切换；这里只留档用于未来双向切换。
    let _ = source;

    (any_ok || !any_fail, notes.join("；"))
}

fn try_audio_to_monitor() -> Result<String, String> {
    let list = crate::platform::kvm_audio_endpoints()?;
    if list.is_empty() {
        return Err("没有可用的音频输出端点".to_string());
    }
    // Windows：form_factor 9 = HDMI/DP（显示器音频）；macOS：名称启发式
    let pick = list
        .iter()
        .find(|(_, _, ff, _)| *ff == 9)
        .or_else(|| {
            list.iter().find(|(_, name, _, _)| {
                let n = name.to_lowercase();
                (n.contains("display") || n.contains("hdmi") || n.contains("monitor")
                    || n.contains("thunderbolt") || n.contains("显示器"))
                    && !n.contains("内置")
                    && !n.contains("internal")
            })
        })
        .or_else(|| list.iter().find(|(_, _, _, is_def)| !*is_def));
    let Some((id, name, _, is_def)) = pick else {
        return Err("未找到显示器音频端点（音频保持不变）".to_string());
    };
    if *is_def {
        return Ok(format!("音频：{} 已是默认输出", name));
    }
    crate::platform::kvm_audio_set_default(id)?;
    Ok(format!("音频：默认输出已切到 {}", name))
}

/// 一键切换（发起侧）。peer_id 为 None 时取唯一已配对对端（多于一个则报错要求指定）。
pub fn switch_impl(peer_id: Option<&str>) -> Result<KvmSwitchReport, String> {
    let rec = {
        let g = peers_slot().lock().map_err(|_| "peers 锁中毒".to_string())?;
        if g.is_empty() {
            return Err("还没有已配对的设备，请先在「切换」页完成配对".to_string());
        }
        match peer_id {
            Some(id) => g
                .iter()
                .find(|r| r.peer.id == id)
                .cloned()
                .ok_or_else(|| format!("未找到设备 {}", id))?,
            None => {
                if g.len() > 1 {
                    return Err(
                        "有多台已配对设备，请在界面选择要切换的目标".to_string()
                    );
                }
                g[0].clone()
            }
        }
    };

    let mut rep = KvmSwitchReport::default();
    let peer = &rec.peer;
    emit_status("progress", &format!("正在切到 {}…", peer.name));

    // ① 读本机剪贴板（超限不发，但要说明）
    let clip = match crate::platform::kvm_clip_get() {
        Some(text) if text.len() > CLIP_LIMIT => {
            rep.clip_msg = "本机剪贴板超过 64KB，未随切换发送".to_string();
            None
        }
        other => other,
    };

    // ② 连对端：上次地址 → 失败则查发现表（IP 可能变了）→ 都没有就是离线
    let mut stream = match connect_peer(&rec) {
        Ok(s) => s,
        Err(e) => {
            emit_status("failed", &e);
            return Err(e);
        }
    };
    // 记住这次成功的地址
    let addr_now = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| peer.addr.clone());

    // ③ 发切换指令（带剪贴板），等 ACK（5 秒）
    let key = hex::decode(&rec.psk).map_err(|_| "本地密钥损坏".to_string())?;
    let send_res = send_frame(
        &mut stream,
        &KvmMsg::Switch {
            source: peer.my_source.clone(),
            clipboard: clip.clone(),
            from: crate::platform::kvm_device_name(),
        },
        Some(&key),
    );
    if let Err(e) = send_res {
        let msg = format!("发送切换指令失败：{}", e);
        emit_status("failed", &msg);
        return Err(msg);
    }
    match recv_ack(&mut stream, &key) {
        Ok((ok, detail)) => {
            rep.ack_ok = ok;
            rep.ack_detail = detail;
        }
        Err(e) => {
            rep.ack_ok = false;
            rep.ack_detail =
                "已发送切换指令，但对方未确认（5 秒超时）。请检查对方应用是否在运行、显示器线缆是否正常".to_string();
            eprintln!("[screenguard] ACK 超时：{}", e);
        }
    }

    // ④ 本机把屏切到对端的输入口（DDC；本机是当前握有画面的一侧，DDC 才可用）
    let (input_ok, input_msg) = crate::platform::kvm_switch_input(&peer.my_source);
    rep.input_ok = input_ok;
    rep.input_msg = input_msg.clone();

    // ⑤ 本机音频切回本机扬声器（把显示器音频让给对端）；失败不影响整体
    rep.audio_msg = try_audio_to_local();

    // ⑥ 更新对端地址
    if addr_now != peer.addr {
        let _ = with_peers(|list| {
            if let Some(r) = list.iter_mut().find(|r| r.peer.fingerprint == peer.fingerprint) {
                r.peer.addr = addr_now;
            }
        });
    }

    let stage = if rep.ack_ok && input_ok {
        "done"
    } else if rep.ack_ok || input_ok {
        "partial"
    } else {
        "failed"
    };
    let msg = if input_ok {
        format!("已切到 {}。{}", peer.name, rep.input_msg)
    } else if rep.ack_ok {
        format!(
            "对方已确认，但本机显示器不支持软件切源：{}。请手动按显示器 Source 键（音频与剪贴板已完成）",
            rep.input_msg
        )
    } else {
        format!("对方未确认：{}（音频/剪贴板可能未完成）", rep.ack_detail)
    };
    emit_status(stage, &msg);
    Ok(rep)
}

fn tcp_addr(ip: &str) -> Result<SocketAddr, String> {
    format!("{}:{}", ip, TCP_PORT)
        .parse()
        .map_err(|_| format!("地址 {} 解析失败", ip))
}

fn connect_peer(rec: &PeerRecord) -> Result<TcpStream, String> {
    let to = Duration::from_millis(1200); // 拔网线的场景必须 1 秒级给出提示
    let last_err;
    // ① 上次地址
    match rec.peer.addr.rsplit_once(':') {
        Some((ip, _)) => match tcp_addr(ip) {
            Ok(a) => match TcpStream::connect_timeout(&a, to) {
                Ok(s) => return Ok(s),
                Err(e) => last_err = format!("{} 不可达（{}）", rec.peer.addr, e),
            },
            Err(e) => last_err = e,
        },
        None => last_err = "对端地址无效".to_string(),
    }
    // ② 发现表里找同名设备（IP 变了的情况）
    if let Ok(g) = discovered_slot().lock() {
        for (ip, (name, _)) in g.iter() {
            if *name == rec.peer.name {
                if let Ok(a) = tcp_addr(ip) {
                    if let Ok(s) = TcpStream::connect_timeout(&a, to) {
                        return Ok(s);
                    }
                }
            }
        }
    }
    Err(format!(
        "对方离线（{}）。请确认对方应用在运行且两台设备在同一局域网",
        last_err
    ))
}

fn recv_ack(stream: &mut TcpStream, key: &[u8]) -> Result<(bool, String), String> {
    stream.set_read_timeout(Some(ACK_TIMEOUT)).ok();
    let resolver = |pid: &str| -> Option<Vec<u8>> {
        let fp = fingerprint_of(key);
        if fp.starts_with(pid) {
            Some(key.to_vec())
        } else {
            None
        }
    };
    let (_, msg) = recv_frame(stream, &resolver)?;
    match msg {
        KvmMsg::Ack { ok, detail } => Ok((ok, detail.unwrap_or_default())),
        other => Err(format!("收到非 ACK 回包：{}", msg_kind(&other))),
    }
}

fn msg_kind(m: &KvmMsg) -> &'static str {
    match m {
        KvmMsg::PairReq { .. } => "PairReq",
        KvmMsg::PairAcc { .. } => "PairAcc",
        KvmMsg::PairRej { .. } => "PairRej",
        KvmMsg::Switch { .. } => "Switch",
        KvmMsg::Ack { .. } => "Ack",
    }
}

fn try_audio_to_local() -> String {
    let list = match crate::platform::kvm_audio_endpoints() {
        Ok(v) => v,
        Err(e) => return format!("音频：{}", e),
    };
    // 挑一个「非显示器音频」的端点让位给对端：Windows ff!=9；macOS 名称启发式
    let pick = list
        .iter()
        .find(|(_, _, ff, _)| *ff != 9 && *ff != 0)
        .or_else(|| {
            list.iter().find(|(_, name, _, _)| {
                let n = name.to_lowercase();
                !n.contains("display") && !n.contains("hdmi") && !n.contains("monitor")
            })
        });
    let Some((id, name, _, is_def)) = pick else {
        return "音频：没有找到本机扬声器端点，保持不变".to_string();
    };
    if *is_def {
        return format!("音频：本机默认输出已是 {}", name);
    }
    match crate::platform::kvm_audio_set_default(id) {
        Ok(()) => format!("音频：本机默认输出已切回 {}", name),
        Err(e) => format!("音频：切回 {} 失败（{}）", name, e),
    }
}

// ---------- 配对 ----------

/// 发起配对：生成 6 位码 + PSK，挂起等待对端输码（5 分钟有效）
pub fn pair_start_impl() -> Result<String, String> {
    let rb = random_bytes(3)?;
    let code = format!(
        "{:02}{:02}{:02}",
        rb[0] % 100, rb[1] % 100, rb[2] % 100
    );
    let mut psk = [0u8; 32];
    psk.copy_from_slice(&random_bytes(32)?);
    let mut g = PENDING.lock().map_err(|_| "配对锁中毒".to_string())?;
    *g = Some((code.clone(), psk, Instant::now() + PENDING_TTL));
    Ok(code)
}

/// 输码完成配对（输入端）：找发起端 → TCP PairReq → 收 PairAcc → 存对端
pub fn pair_join_impl(code: &str) -> Result<KvmPeer, String> {
    let code = code.trim().to_string();
    if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
        return Err("配对码是 6 位数字".to_string());
    }
    // 找发起端：发现表里最近的条目；没有就等发现轮（最多 4 秒）
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut last_err = String::new();
    loop {
        let candidate = {
            let g = discovered_slot().lock().map_err(|_| "发现表锁中毒".to_string())?;
            g.iter()
                .max_by_key(|(_, (_, t))| *t)
                .map(|(ip, (name, _))| (ip.clone(), name.clone()))
        };
        if let Some((ip, _name)) = candidate {
            match join_addr(&ip, &code) {
                Ok(peer) => return Ok(peer),
                Err(e) => last_err = e,
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    Err(if last_err.is_empty() {
        "局域网里没有发现其他运行本应用的设备（对方应用需要在前台/后台运行）".to_string()
    } else {
        last_err
    })
}

fn join_addr(ip: &str, code: &str) -> Result<KvmPeer, String> {
    let addr = tcp_addr(ip)?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))
        .map_err(|_| format!("连接 {} 失败（对方可能不在线）", ip))?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    send_frame(
        &mut stream,
        &KvmMsg::PairReq {
            code: code.to_string(),
            name: crate::platform::kvm_device_name(),
        },
        None,
    )?;
    let (_, msg) = recv_frame(&mut stream, &|_| None)?;
    match msg {
        KvmMsg::PairAcc { psk, name } => {
            if psk.len() != 64 {
                return Err("对端下发的密钥格式异常".to_string());
            }
            let psk_bytes = hex::decode(&psk).map_err(|_| "密钥解析失败".to_string())?;
            let peer = KvmPeer {
                id: hex::encode(random_bytes(8)?),
                name: sanitize_name(&name),
                addr: format!("{}:{}", ip, TCP_PORT),
                fingerprint: fingerprint_of(&psk_bytes),
                my_source: default_source(),
                paired_at: crate::platform::now_str(),
            };
            let rec = PeerRecord { peer: peer.clone(), psk };
            with_peers(|list| {
                if let Some(old) = list.iter_mut().find(|r| r.peer.fingerprint == rec.peer.fingerprint) {
                    old.peer = rec.peer.clone();
                    old.psk = rec.psk.clone();
                } else {
                    list.push(rec.clone());
                }
            })?;
            emit_status("info", &format!("已与 {} 完成配对", peer.name));
            Ok(peer)
        }
        KvmMsg::PairRej { reason } => Err(reason),
        other => Err(format!("配对响应异常（{}）", msg_kind(&other))),
    }
}

// ---------- 设备管理 ----------

pub fn devices_impl() -> KvmDevices {
    let peers: Vec<KvmPeer> = peers_slot()
        .lock()
        .map(|g| g.iter().map(|r| r.peer.clone()).collect())
        .unwrap_or_default();
    let peer_names: Vec<String> = peers.iter().map(|p| p.name.clone()).collect();
    let mut discovered = Vec::new();
    if let Ok(g) = discovered_slot().lock() {
        for (ip, (name, seen)) in g.iter() {
            if seen.elapsed() < Duration::from_secs(FRESH_SECS)
                && !peer_names.contains(name)
            {
                discovered.push(KvmDevice { name: name.clone(), ip: ip.clone() });
            }
        }
    }
    discovered.sort_by(|a, b| a.name.cmp(&b.name));
    KvmDevices {
        me: crate::platform::kvm_device_name(),
        peers,
        discovered,
    }
}

pub fn peer_remove_impl(id: &str) -> Result<(), String> {
    with_peers(|list| list.retain(|r| r.peer.id != id))?;
    emit_status("info", "已解除一台设备的配对");
    Ok(())
}

pub fn peer_set_source_impl(id: &str, source: &str) -> Result<(), String> {
    let s = source.trim().to_lowercase();
    let valid = ["dp", "hdmi1", "hdmi2", "usbc", "15", "17", "18", "27"];
    if !valid.contains(&s.as_str()) {
        return Err("输入口只能是 dp / hdmi1 / hdmi2 / usbc".to_string());
    }
    with_peers(|list| {
        let r = list
            .iter_mut()
            .find(|r| r.peer.id == id)
            .ok_or_else(|| format!("未找到设备 {}", id))?;
        r.peer.my_source = s;
        Ok::<(), String>(())
    })??;
    Ok(())
}

pub fn unpair_all_impl() -> Result<(), String> {
    with_peers(|list| {
        list.clear();
        Ok::<(), String>(())
    })??;
    emit_status("info", "已解除所有配对");
    Ok(())
}

/// 热键入口：无对端时静默轻提示，不弹错误
pub fn switch_from_hotkey() {
    match switch_impl(None) {
        Ok(_) => {}
        Err(e) => emit_status("failed", &e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip_plain_and_encrypted() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let psk: Vec<u8> = (0..32).collect();
        let psk_rx = psk.clone();
        let t = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let resolver = move |pid: &str| -> Option<Vec<u8>> {
                if fingerprint_of(&psk_rx).starts_with(pid) {
                    Some(psk_rx.clone())
                } else {
                    None
                }
            };
            let m1 = recv_frame(&mut s, &resolver).unwrap();
            let m2 = recv_frame(&mut s, &resolver).unwrap();
            let m3 = recv_frame(&mut s, &resolver).unwrap();
            (m1, m2, m3)
        });
        let mut c = TcpStream::connect(addr).unwrap();
        send_frame(&mut c, &KvmMsg::PairReq { code: "123456".into(), name: "测试机".into() }, None).unwrap();
        send_frame(
            &mut c,
            &KvmMsg::Switch {
                source: "hdmi1".into(),
                clipboard: Some("hello 剪贴板 ✓".into()),
                from: "A".into(),
            },
            Some(&psk),
        )
        .unwrap();
        send_frame(&mut c, &KvmMsg::Ack { ok: true, detail: Some("明细".into()) }, Some(&psk)).unwrap();
        let ((pid1, m1), (pid2, m2), (pid3, m3)) = t.join().unwrap();
        assert!(pid1.is_empty(), "明文帧无 pid");
        assert!(matches!(m1, KvmMsg::PairReq { ref code, .. } if code == "123456"));
        let fp = fingerprint_of(&psk);
        assert_eq!(pid2, fp[..8], "加密帧 pid = 指纹前 8 位");
        assert!(matches!(&m2, KvmMsg::Switch { clipboard: Some(text), .. } if text == "hello 剪贴板 ✓"));
        assert!(matches!(m3, KvmMsg::Ack { ok: true, .. }));
    }

    #[test]
    fn frame_rejects_bad_magic_and_wrong_key() {
        // 错误密钥：解密必须失败，绝不静默通过
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let good: Vec<u8> = vec![7u8; 32];
        let bad: Vec<u8> = vec![9u8; 32];
        let t = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let resolver = move |pid: &str| -> Option<Vec<u8>> {
                if fingerprint_of(&bad).starts_with(pid) {
                    Some(bad.clone())
                } else {
                    None
                }
            };
            recv_frame(&mut s, &resolver)
        });
        let mut c = TcpStream::connect(addr).unwrap();
        send_frame(&mut c, &KvmMsg::PairReq { code: "000000".into(), name: "x".into() }, Some(&good))
            .unwrap();
        let r = t.join().unwrap();
        assert!(r.is_err(), "密钥不符必须解密失败");
    }

    #[test]
    fn sanitize_name_strips_control_and_truncates() {
        assert_eq!(sanitize_name("DESKTOP-ABC\u{0}\u{7}"), "DESKTOP-ABC");
        let long = "x".repeat(100);
        assert_eq!(sanitize_name(&long).len(), 64);
        assert_eq!(sanitize_name("  "), "");
    }

    #[test]
    fn pair_start_generates_six_digit_code() {
        let code = pair_start_impl().unwrap();
        assert_eq!(code.len(), 6, "码长 6：{}", code);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
        // 清理：不留悬挂的 pending
        let _ = PENDING.lock().unwrap().take();
    }

    #[test]
    fn peer_set_source_validates() {
        // 无 peers 时空列表会直接报未找到——这条只测校验分支
        let r = peer_set_source_impl("no-such-id", "vga");
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("输入口"));
    }

    /// 真机回环：起真实 TCP 服务 → 发起配对 → 本机输码配对成功，双方各存记录。
    /// 会短暂触碰真实 peers.json：先备份、测完恢复。
    #[test]
    #[ignore]
    fn kvm_pair_loopback_smoke() {
        let path = peers_file();
        let backup = path.with_extension("json.kvm-bak");
        let had_user = path.exists();
        if had_user {
            std::fs::copy(&path, &backup).expect("备份 peers.json 失败");
        }
        ensure_started();
        // TCP 服务线程起来（bind 是同步的，但线程调度需要一点时间）
        std::thread::sleep(Duration::from_millis(400));
        let code = pair_start_impl().unwrap();
        let peer = join_addr("127.0.0.1", &code).unwrap_or_else(|e| {
            panic!("回环配对失败：{}", e);
        });
        assert_eq!(peer.fingerprint.len(), 16);
        // 双方（同进程两条路径）都应各有一条记录
        let devs = devices_impl();
        assert!(
            devs.peers.iter().any(|p| p.fingerprint == peer.fingerprint),
            "对端记录应已保存"
        );
        // 清理：恢复用户数据
        if had_user {
            std::fs::rename(&backup, &path).ok();
        } else {
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(&backup);
        }
        println!(
            "kvm_pair_loopback_smoke 全部通过（配对到：{}，指纹 {}）",
            peer.name, peer.fingerprint
        );
    }
}
