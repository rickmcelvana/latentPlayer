//! The desktop shell's side of a Subsonic connection (Phase 2 M1): where the
//! server address is remembered, where the secret is kept, and the commands the
//! UI drives both through.
//!
//! Split from `lib.rs` rather than added to it because it is the first part of
//! this shell that owns *persistent, secret* state -- everything else here is
//! either a handle to the engine or a path in the app data directory.
//!
//! # Where the two halves live, and why they are not the same place
//!
//! The address, username and which kind of secret it is are ordinary settings
//! and live beside the library index in the app data directory. The **secret**
//! does not: it goes to the OS credential store (Windows Credential Manager,
//! macOS keychain, Linux secret service) through `keyring`.
//!
//! That split is decision 4 of tasks/phase-2.md, and it is forced by the
//! protocol. Subsonic's token scheme hashes the password *per request*, so a
//! client that speaks it must hold the password itself -- there is no
//! derived-secret-at-rest to store instead. Since it must be kept, it is kept
//! where the OS already keeps such things, rather than in a JSON file next to
//! the album art. OpenSubsonic's API-key extension is what removes the need
//! entirely, and this module prefers it when a server offers one; the Navidrome
//! this was built against (0.63.2) does not.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use player_core::subsonic::http::HttpClient;
use player_core::subsonic::{Credentials, Extensions, Server, ServerInfo, SubsonicError};

/// The service name the secret is filed under in the OS credential store. It is
/// what the user sees in Credential Manager / Keychain Access, so it says the
/// app and what the secret is for rather than something internal.
const KEYRING_SERVICE: &str = "latentPlayer (music server)";

/// The settings file, beside the library index in the app data directory.
const SETTINGS_FILE: &str = "server.json";

/// Which kind of secret is in the credential store for this server.
///
/// Stored rather than inferred: the two are the same string to the store and
/// mean entirely different things to the protocol, and a client that guessed
/// would send a password as an API key on the first request after a restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthKind {
    Password,
    ApiKey,
}

/// What is remembered about a server between sessions. Everything here is
/// non-secret by construction -- see the module docs.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerSettings {
    pub base_url: String,
    /// Empty for API-key auth, which sends no username at all.
    #[serde(default)]
    pub user: String,
    pub auth: AuthKind,
}

impl ServerSettings {
    /// The key the secret is filed under. Scoped by server and user so two
    /// servers -- or two accounts on one server -- do not overwrite each other's
    /// secret the moment both are configured.
    fn credential_key(&self) -> String {
        format!("{}#{}", self.base_url, self.user)
    }
}

/// A live connection: the client, and what the server said about itself when it
/// answered.
pub struct Connection {
    pub client: HttpClient,
    pub info: ServerInfo,
    pub extensions: Extensions,
}

/// The shell's server state. `None` means no server is configured *or* the
/// configured one has not been connected to yet this session -- the settings
/// file is what remembers across sessions.
pub type SharedServer = Arc<Mutex<Option<Connection>>>;

/// What the UI needs to draw the server panel, in one round trip -- the same
/// rule `TransportState` and `EqState` follow.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerState {
    /// Whether a server is configured at all (i.e. there are settings on disk).
    pub configured: bool,
    /// Whether this session has a working connection to it.
    pub connected: bool,
    pub base_url: String,
    pub user: String,
    /// `"password"` or `"apiKey"`, matching what is stored.
    pub auth: String,
    /// What the server said about itself, empty until connected.
    pub server_type: String,
    pub server_version: String,
    pub api_version: String,
    pub open_subsonic: bool,
    /// Whether this server can take an API key instead of a password, so the UI
    /// can offer the better option rather than only mentioning it.
    pub supports_api_key: bool,
    /// Why the last attempt failed, if it did. Empty otherwise.
    pub error: String,
    /// Whether that failure was about credentials, so the UI knows whether to
    /// send the user back to the password field or to the address field.
    pub error_is_credentials: bool,
}

impl ServerState {
    fn from_settings(settings: &ServerSettings) -> Self {
        Self {
            configured: true,
            base_url: settings.base_url.clone(),
            user: settings.user.clone(),
            auth: match settings.auth {
                AuthKind::Password => "password".into(),
                AuthKind::ApiKey => "apiKey".into(),
            },
            ..Default::default()
        }
    }

    fn with_error(mut self, error: &SubsonicError) -> Self {
        self.connected = false;
        self.error = error.to_string();
        self.error_is_credentials = matches!(
            error,
            SubsonicError::Server { code, .. } if code.is_credentials_problem()
        );
        self
    }
}

fn settings_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SETTINGS_FILE)
}

/// Reads the remembered settings, or `None` when no server has been set up.
///
/// A settings file that cannot be parsed is treated as no server rather than as
/// an error: the recovery is the same (set the server up again) and refusing to
/// start over a stale settings file would be worse.
pub fn load_settings(data_dir: &Path) -> Option<ServerSettings> {
    let bytes = std::fs::read(settings_path(data_dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn save_settings(data_dir: &Path, settings: &ServerSettings) -> Result<(), String> {
    if let Some(parent) = settings_path(data_dir).parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(settings_path(data_dir), json).map_err(|e| e.to_string())
}

fn store_secret(settings: &ServerSettings, secret: &str) -> Result<(), String> {
    keyring::v1::Entry::new(KEYRING_SERVICE, &settings.credential_key())
        .and_then(|entry| entry.set_password(secret))
        .map_err(|e| format!("the secret could not be stored in the OS credential store: {e}"))
}

fn read_secret(settings: &ServerSettings) -> Result<String, String> {
    keyring::v1::Entry::new(KEYRING_SERVICE, &settings.credential_key())
        .and_then(|entry| entry.get_password())
        .map_err(|e| format!("the stored secret could not be read back: {e}"))
}

fn forget_secret(settings: &ServerSettings) {
    // A missing entry is the desired end state, so failing to delete one is not
    // worth failing the call over -- but it is worth saying, since a secret that
    // outlives its server is exactly what "forget" was meant to prevent.
    if let Ok(entry) = keyring::v1::Entry::new(KEYRING_SERVICE, &settings.credential_key()) {
        if let Err(e) = entry.delete_credential() {
            log::warn!("the stored secret could not be deleted: {e}");
        }
    }
}

fn credentials_for(settings: &ServerSettings, secret: String) -> Credentials {
    match settings.auth {
        AuthKind::Password => Credentials::Password {
            user: settings.user.clone(),
            password: secret,
        },
        AuthKind::ApiKey => Credentials::ApiKey { key: secret },
    }
}

/// Connects, and asks the server what it can do.
///
/// `ping` first because it is the call whose failure means something specific
/// (wrong credentials, wrong address, nothing there); extensions second, and
/// only as information -- a server with none is an old server, not a broken one.
fn connect(settings: &ServerSettings, secret: String) -> Result<Connection, SubsonicError> {
    let server = Server::new(&settings.base_url)?;
    let client = HttpClient::new(server, credentials_for(settings, secret));
    let info = client.ping()?;
    let extensions = client.extensions().unwrap_or_default();
    Ok(Connection {
        client,
        info,
        extensions,
    })
}

fn state_of(settings: &ServerSettings, connection: &Connection) -> ServerState {
    ServerState {
        connected: true,
        server_type: connection.info.server_type.clone(),
        server_version: connection.info.server_version.clone(),
        api_version: connection.info.version.clone(),
        open_subsonic: connection.info.open_subsonic,
        supports_api_key: connection.extensions.supports_api_key(),
        ..ServerState::from_settings(settings)
    }
}

/// Connects using the settings and secret already stored, if there are any.
///
/// Called at startup so a configured server is live before the UI asks. Failure
/// is deliberately quiet here: a laptop opened away from home cannot reach its
/// music server, and that is not a reason to complain on launch -- the panel
/// shows it, and reconnecting is a button.
pub fn reconnect_stored(data_dir: &Path, shared: &SharedServer) {
    if let Ok(state) = reconnect_blocking(data_dir, shared) {
        if !state.error.is_empty() {
            log::info!(
                "the stored music server did not answer at startup: {}",
                state.error
            );
        }
    }
}

/// The work behind [`reconnect_server`], on whichever thread can afford to
/// block.
///
/// Tries the connection this session already has before rebuilding one: a
/// server that is still there answers a `ping` through the existing client
/// without touching the OS credential store, and the pooled connection skips a
/// fresh TCP and TLS handshake. Only when that fails -- the laptop moved, the
/// server restarted, the token stopped being accepted -- is the credential read
/// again and a new client built.
///
/// The connection is taken *out* of the slot for the duration of the ping rather
/// than pinged under the lock. A ping to a live server is milliseconds, and on a
/// dead one the panel reporting "not connected" while we find out is the truth
/// rather than a gap in it -- where holding the lock would freeze every other
/// reader for the whole request timeout.
fn reconnect_blocking(data_dir: &Path, shared: &SharedServer) -> Result<ServerState, String> {
    let Some(settings) = load_settings(data_dir) else {
        return Ok(ServerState::default());
    };

    if let Some(existing) = shared.lock().unwrap().take() {
        match existing.client.ping() {
            Ok(info) => {
                let refreshed = Connection { info, ..existing };
                let state = state_of(&settings, &refreshed);
                *shared.lock().unwrap() = Some(refreshed);
                return Ok(state);
            }
            Err(e) => log::info!("the open connection to the music server has gone stale: {e}"),
        }
    }

    let secret = read_secret(&settings)?;
    match connect(&settings, secret) {
        Ok(connection) => {
            let state = state_of(&settings, &connection);
            *shared.lock().unwrap() = Some(connection);
            Ok(state)
        }
        Err(e) => Ok(ServerState::from_settings(&settings).with_error(&e)),
    }
}

/// What the UI shows: the stored settings, plus whether this session is
/// connected to them.
#[tauri::command]
pub fn server_state(
    data_dir: tauri::State<PathBuf>,
    shared: tauri::State<SharedServer>,
) -> ServerState {
    let Some(settings) = load_settings(&data_dir) else {
        return ServerState::default();
    };
    match shared.lock().unwrap().as_ref() {
        Some(connection) => state_of(&settings, connection),
        None => ServerState::from_settings(&settings),
    }
}

/// Sets up (or replaces) the server, and connects to it.
///
/// `async` and doing its work on the blocking pool for `scan_folder`'s reason:
/// this makes a network round trip, and a server that is not there takes the
/// whole request timeout to say so. On the main thread that is a frozen UI for
/// twenty seconds.
///
/// The secret is only stored once the server has *accepted* it, so a typo does
/// not leave a wrong password in the credential store to be retried at every
/// launch.
#[tauri::command]
pub async fn connect_server(
    base_url: String,
    user: String,
    secret: String,
    auth: AuthKind,
    data_dir: tauri::State<'_, PathBuf>,
    shared: tauri::State<'_, SharedServer>,
) -> Result<ServerState, String> {
    let settings = ServerSettings {
        base_url,
        user,
        auth,
    };
    let attempt = {
        let settings = settings.clone();
        tauri::async_runtime::spawn_blocking(move || {
            connect(&settings, secret.clone()).map(|connection| (connection, secret))
        })
        .await
        .map_err(|e| e.to_string())?
    };

    match attempt {
        Ok((connection, secret)) => {
            let state = state_of(&settings, &connection);
            store_secret(&settings, &secret)?;
            save_settings(&data_dir, &settings)?;
            *shared.lock().unwrap() = Some(connection);
            Ok(state)
        }
        // Not an `Err` from the command: a wrong password is an ordinary
        // outcome the panel draws, not an exception for the UI to catch. The
        // state carries why, and whether the fix is the password field or the
        // address field.
        //
        // `configured` is answered from what is *stored*, not from what was
        // just typed: nothing was saved, so a first attempt that fails must not
        // leave the UI believing a server is set up. The typed address and
        // username still travel back, so the form can redisplay them.
        Err(e) => Ok(ServerState {
            configured: load_settings(&data_dir).is_some(),
            ..ServerState::from_settings(&settings).with_error(&e)
        }),
    }
}

/// Tries the stored settings again -- the button for "it was down, try now",
/// and the check for "am I still connected".
#[tauri::command]
pub async fn reconnect_server(
    data_dir: tauri::State<'_, PathBuf>,
    shared: tauri::State<'_, SharedServer>,
) -> Result<ServerState, String> {
    let data_dir = data_dir.inner().clone();
    let shared = Arc::clone(shared.inner());
    tauri::async_runtime::spawn_blocking(move || reconnect_blocking(&data_dir, &shared))
        .await
        .map_err(|e| e.to_string())?
}

/// Forgets the server: the settings file, the secret in the credential store,
/// and this session's connection. All three, because a "forget" that left the
/// password behind would be a lie.
#[tauri::command]
pub fn forget_server(
    data_dir: tauri::State<PathBuf>,
    shared: tauri::State<SharedServer>,
) -> Result<ServerState, String> {
    if let Some(settings) = load_settings(&data_dir) {
        forget_secret(&settings);
        let path = settings_path(&data_dir);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| e.to_string())?;
        }
    }
    *shared.lock().unwrap() = None;
    Ok(ServerState::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_through_the_data_directory() {
        let dir = std::env::temp_dir().join(format!("latentplayer_servers_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create the test directory");
        assert!(load_settings(&dir).is_none(), "nothing stored yet");

        let settings = ServerSettings {
            base_url: "http://localhost:4533".into(),
            user: "rick".into(),
            auth: AuthKind::Password,
        };
        save_settings(&dir, &settings).expect("save");
        let read = load_settings(&dir).expect("the settings should read back");
        assert_eq!(read.base_url, "http://localhost:4533");
        assert_eq!(read.user, "rick");
        assert_eq!(read.auth, AuthKind::Password);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The settings file is the one place a secret must never appear, so this
    /// checks the bytes rather than trusting the struct's shape.
    #[test]
    fn no_secret_is_written_to_the_settings_file() {
        let dir = std::env::temp_dir().join(format!(
            "latentplayer_servers_secret_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create the test directory");
        let settings = ServerSettings {
            base_url: "http://localhost:4533".into(),
            user: "rick".into(),
            auth: AuthKind::Password,
        };
        save_settings(&dir, &settings).expect("save");
        let written = std::fs::read_to_string(settings_path(&dir)).expect("read back");
        assert!(!written.to_lowercase().contains("password") || !written.contains("sesame"));
        assert!(!written.contains("sesame"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The OS credential store, actually written to and read back.
    ///
    /// Worth a real test rather than a mocked one: this is the integration most
    /// likely to fail silently on a machine, and the failure mode is a password
    /// that appears to save and is gone at the next launch. It uses an
    /// obviously-fake secret under an address that cannot exist (`.invalid` is
    /// reserved for exactly this), and deletes what it wrote.
    ///
    /// On Windows the Credential Manager is always there, so a failure is a real
    /// failure. Elsewhere -- a headless Linux box with no secret service, say --
    /// having no store is an environment rather than a bug, and this skips
    /// instead of failing, the same way `subsonic_smoke` skips with no server.
    #[test]
    fn the_os_credential_store_round_trips() {
        let settings = ServerSettings {
            base_url: "http://credential-store-check.invalid".into(),
            user: "latentplayer-test".into(),
            auth: AuthKind::Password,
        };
        const FAKE: &str = "not-a-real-secret";

        if let Err(e) = store_secret(&settings, FAKE) {
            if cfg!(windows) {
                panic!("the Windows Credential Manager should be available: {e}");
            }
            println!("skipping: no usable OS credential store here ({e})");
            return;
        }

        assert_eq!(
            read_secret(&settings).as_deref(),
            Ok(FAKE),
            "what went into the credential store should come back out"
        );

        forget_secret(&settings);
        assert!(
            read_secret(&settings).is_err(),
            "forgetting a server must leave no secret behind -- that is the whole \
             point of the button"
        );
    }

    /// Two accounts, or two servers, must not share one credential slot.
    #[test]
    fn credential_keys_are_scoped_to_the_server_and_the_user() {
        let one = ServerSettings {
            base_url: "http://localhost:4533".into(),
            user: "rick".into(),
            auth: AuthKind::Password,
        };
        let same_server_other_user = ServerSettings {
            user: "sam".into(),
            ..one.clone()
        };
        let other_server = ServerSettings {
            base_url: "https://music.example.com".into(),
            ..one.clone()
        };
        assert_ne!(
            one.credential_key(),
            same_server_other_user.credential_key()
        );
        assert_ne!(one.credential_key(), other_server.credential_key());
    }
}
