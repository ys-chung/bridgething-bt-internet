//! Headless bridgething gateway over Bluetooth RFCOMM.
//!
//! Dials the Car Thing's bridgething RFCOMM service and runs the same shared
//! companion core the phone and desktop apps use, so the device can reach the
//! internet (the `Tunnel` surface) through this host when no phone is around.

use std::{
  collections::HashMap,
  path::{Path, PathBuf},
  sync::{Arc, Mutex},
  time::Duration,
};

use anyhow::Context;
use bridgething_companion::{
  api::{CapabilityFlags, CompanionBackends, CompanionConfig, CompanionSession, HostInfo, SessionEvent, SessionEventSink},
  backend::{HostClock, HostEnvironment, LinkDevice, LogLevel, LogSink, NativeHttp, NativeWs, SecretStore},
};
use bridgething_gateway::transport::FramedConnector;
use tracing_subscriber::EnvFilter;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const FIRST_RETRY: Duration = Duration::from_secs(2);
const RETRY_CAP: Duration = Duration::from_secs(30);
const LINK_POLL: Duration = Duration::from_secs(2);
const BRING_UP_GRACE: Duration = Duration::from_secs(15);

struct Settings {
  mac: String,
  channel: u8,
  data_dir: PathBuf,
  name: String,
}

impl Settings {
  fn from_env() -> anyhow::Result<Self> {
    let mac = std::env::var("CARTHING_MAC").context("CARTHING_MAC must be set to the Car Thing's Bluetooth address")?;
    let channel = std::env::var("RFCOMM_CHANNEL")
      .ok()
      .map(|raw| raw.parse())
      .transpose()
      .context("RFCOMM_CHANNEL must be a number")?
      .unwrap_or(1);
    let data_dir = std::env::var_os("DATA_DIR").map_or_else(|| PathBuf::from("/data"), PathBuf::from);
    let name = std::env::var("GATEWAY_NAME").unwrap_or_else(|_| "bt-gateway".to_owned());
    Ok(Self {
      mac,
      channel,
      data_dir,
      name,
    })
  }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  tracing_subscriber::fmt()
    .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
    .init();

  let settings = Settings::from_env()?;
  let session = create_session(&settings)?;
  session.start().await?;
  tracing::info!(mac = %settings.mac, channel = settings.channel, "gateway session started");

  tokio::select! {
    result = keep_linked(&settings, &session) => result,
    _ = tokio::signal::ctrl_c() => {
      tracing::info!("shutting down");
      Ok(())
    }
  }
}

async fn keep_linked(settings: &Settings, session: &Arc<CompanionSession>) -> anyhow::Result<()> {
  let device_id = format!("bt:{}", settings.mac);
  let mut backoff = FIRST_RETRY;
  let mut failures: u32 = 0;
  loop {
    // The kernel can leave an RFCOMM connect pending for minutes after the
    // link drops; dropping the future closes the socket so the next attempt
    // pages the Car Thing afresh.
    let attempt = tokio::time::timeout(CONNECT_TIMEOUT, rfcomm::connect(&settings.mac, settings.channel)).await;
    match attempt.unwrap_or_else(|_| Err(anyhow::anyhow!("connect timed out after {CONNECT_TIMEOUT:?}"))) {
      Ok(stream) => {
        tracing::info!(failed_attempts = failures, "rfcomm link to the car thing is up");
        failures = 0;
        let device = LinkDevice {
          id: device_id.clone(),
          name: "car thing".to_owned(),
        };
        session.connect_direct(device, FramedConnector::new(stream)).await;
        if held(session, &device_id).await {
          backoff = FIRST_RETRY;
        }
        session.disconnect_direct(&device_id).await;
        tracing::info!("rfcomm link to the car thing is down");
      }
      Err(error) => {
        failures += 1;
        // Log the first failure and then every 20th, so a long absence stays quiet.
        if failures == 1 || failures % 20 == 0 {
          tracing::info!(%error, failed_attempts = failures, "car thing not reachable; retrying");
        } else {
          tracing::debug!(%error, failed_attempts = failures, "car thing not reachable");
        }
      }
    }
    tokio::time::sleep(backoff).await;
    backoff = (backoff * 2).min(RETRY_CAP);
  }
}

/// Waits for the link to come up and then go away again. Returns whether it was ever up.
async fn held(session: &Arc<CompanionSession>, device_id: &str) -> bool {
  let inner = session.session();
  let deadline = tokio::time::Instant::now() + BRING_UP_GRACE;
  while !inner.is_linked(device_id) {
    if tokio::time::Instant::now() >= deadline {
      tracing::warn!("the companion core never brought the link up");
      return false;
    }
    tokio::time::sleep(LINK_POLL).await;
  }
  while inner.is_linked(device_id) {
    tokio::time::sleep(LINK_POLL).await;
  }
  true
}

fn create_session(settings: &Settings) -> anyhow::Result<Arc<CompanionSession>> {
  let state_dir = settings.data_dir.join("state");
  let cache_dir = settings.data_dir.join("cache");
  for dir in [&state_dir, &cache_dir] {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
  }

  let backends = CompanionBackends {
    link: None,
    host: Arc::new(Host),
    http: Arc::new(NativeHttp::default()),
    ws: Arc::new(NativeWs::new()),
    secrets: Arc::new(FileSecrets::open(&settings.data_dir.join("secrets.json"))),
    log: Arc::new(TracingLog),
    audio: None,
    volume: None,
    geo: None,
    notifications: None,
    phone: None,
    media_sessions: None,
    stream: None,
    speech: None,
    nlu: None,
    apple_music: None,
    image: None,
    model_validator: None,
    transfer_policy: None,
    connectivity: None,
    device_waker: None,
    extensions: None,
  };

  let config = CompanionConfig {
    host: HostInfo {
      app_name: settings.name.clone(),
      app_version: env!("CARGO_PKG_VERSION").to_owned(),
      os_name: std::env::consts::OS.to_owned(),
      os_version: String::new(),
      host_identifier: host_identifier(&settings.data_dir),
    },
    capabilities: CapabilityFlags {
      geo: false,
      notifications: false,
      net_fetch: true,
      net_ws: true,
      audio_tts: false,
      voice_model: false,
    },
    state_dir: state_dir.to_string_lossy().into_owned(),
    cache_dir: cache_dir.to_string_lossy().into_owned(),
    model_platform: None,
    spotify: None,
  };

  Ok(CompanionSession::create(config, backends, Arc::new(EventLog)))
}

fn host_identifier(data_dir: &Path) -> String {
  let path = data_dir.join("host-id");
  if let Ok(held) = std::fs::read_to_string(&path)
    && !held.trim().is_empty()
  {
    return held.trim().to_owned();
  }
  let fresh = uuid::Uuid::now_v7().to_string();
  if let Err(error) = std::fs::write(&path, &fresh) {
    tracing::warn!(%error, path = %path.display(), "the host identifier could not be kept");
  }
  fresh
}

struct Host;

impl HostEnvironment for Host {
  fn clock(&self) -> HostClock {
    let now = chrono::Local::now();
    HostClock {
      tz_iana: iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_owned()),
      locale: "en-US".to_owned(),
      unix_seconds: now.timestamp().max(0) as u64,
      utc_offset_minutes: (now.offset().local_minus_utc() / 60) as i16,
      dst_offset_minutes: 0,
    }
  }
}

struct FileSecrets {
  path: PathBuf,
  held: Mutex<HashMap<String, String>>,
}

impl FileSecrets {
  fn open(path: &Path) -> Self {
    let held = std::fs::read(path)
      .ok()
      .and_then(|raw| serde_json::from_slice(&raw).ok())
      .unwrap_or_default();
    Self {
      path: path.to_owned(),
      held: Mutex::new(held),
    }
  }

  fn write(&self, change: impl FnOnce(&mut HashMap<String, String>)) {
    let mut held = self.held.lock().unwrap();
    change(&mut held);
    match serde_json::to_vec(&*held) {
      Ok(raw) => {
        if let Err(error) = std::fs::write(&self.path, raw) {
          tracing::warn!(%error, "secrets could not be saved");
        }
      }
      Err(error) => tracing::warn!(%error, "secrets could not be encoded"),
    }
  }
}

impl SecretStore for FileSecrets {
  fn get(&self, key: String) -> Option<String> {
    self.held.lock().unwrap().get(&key).cloned()
  }

  fn set(&self, key: String, value: String) {
    self.write(|held| {
      held.insert(key, value);
    });
  }

  fn remove(&self, key: String) {
    self.write(|held| {
      held.remove(&key);
    });
  }

  fn get_blob(&self, key: String) -> Option<Vec<u8>> {
    self.get(key).map(String::into_bytes)
  }
}

struct TracingLog;

impl LogSink for TracingLog {
  fn on_line(&self, level: LogLevel, target: String, message: String) {
    match level {
      LogLevel::Trace => tracing::trace!(target: "bridgething::core", %target, "{message}"),
      LogLevel::Debug => tracing::debug!(target: "bridgething::core", %target, "{message}"),
      LogLevel::Info => tracing::info!(target: "bridgething::core", %target, "{message}"),
      LogLevel::Warn => tracing::warn!(target: "bridgething::core", %target, "{message}"),
      LogLevel::Error => tracing::error!(target: "bridgething::core", %target, "{message}"),
    }
  }
}

struct EventLog;

impl SessionEventSink for EventLog {
  fn on_event(&self, event: SessionEvent) {
    match event {
      SessionEvent::PeerConnected { peer } => tracing::info!(id = %peer.id, "device linked"),
      SessionEvent::PeerDisconnected { device_id } => tracing::info!(id = %device_id, "device unlinked"),
      SessionEvent::PeerLinkFailed { peer } => {
        tracing::warn!(id = %peer.id, error = ?peer.link_error, "device link failed");
      }
      _ => {}
    }
  }
}

#[cfg(target_os = "linux")]
mod rfcomm {
  use bluer::{
    Address,
    rfcomm::{SocketAddr, Stream},
  };

  pub async fn connect(mac: &str, channel: u8) -> anyhow::Result<Stream> {
    let address: Address = mac.parse()?;
    Ok(Stream::connect(SocketAddr::new(address, channel)).await?)
  }
}

#[cfg(not(target_os = "linux"))]
mod rfcomm {
  pub async fn connect(_mac: &str, _channel: u8) -> anyhow::Result<tokio::io::DuplexStream> {
    anyhow::bail!("RFCOMM is only supported on Linux")
  }
}
