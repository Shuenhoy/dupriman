//! Private file protocol between the CUPS backend and the graphical prompt.
//!
//! Production installations provision a root-owned state layout for every
//! allowed user.  A dedicated `dupriman-<user>` group bridges the unprivileged
//! account cupsd runs backends as and that one user without exposing job
//! metadata to other users.  Upstream CUPS uses `lp`; some distributions
//! (Arch) use `cups`, so the backend account is detected rather than assumed.
//! Direct single-user test runs retain a private 0700 directory layout.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// Accounts cupsd may run backends as, in preference order: upstream CUPS
/// default first, then the Arch Linux / Fedora convention.
const BACKEND_ACCOUNTS: &[&str] = &["lp", "cups"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptRequest {
    pub id: String,
    pub capability: String,
    pub job_id: String,
    pub user: String,
    pub title: String,
    pub detail: String,
    /// PID of the backend waiting for this answer.  CUPS can kill a backend
    /// (job cancelled, queue disabled) without giving it a chance to clean up,
    /// and the prompt directory is not writable by the desktop user, so the
    /// request file would otherwise stay behind and re-open a dialog for a job
    /// that no longer exists.  0 means "unknown" (older or test requests).
    #[serde(default)]
    pub pid: u32,
}

impl PromptRequest {
    /// Whether the backend that asked is still alive.
    pub fn is_live(&self) -> bool {
        self.pid == 0 || Path::new(&format!("/proc/{}", self.pid)).exists()
    }
}

/// Whether a request file is older than `max_age`.
///
/// A backend that CUPS killed cannot clean up after itself, and the desktop
/// user has no write permission on `requests/`, so a leftover file otherwise
/// re-opens its dialog forever.  Requests written by older versions carry no
/// pid, which makes age the only available signal.
fn is_stale(path: &Path, max_age: Option<Duration>) -> bool {
    let Some(max_age) = max_age else {
        return false;
    };
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age > max_age)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptResponse {
    pub id: String,
    pub capability: String,
    pub confirmed: bool,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitOutcome {
    Confirmed,
    Declined(String),
    Timeout,
}

#[derive(Debug, Clone, Copy)]
pub struct UserIdentity {
    uid: u32,
    gid: u32,
}

#[derive(Debug, Clone)]
pub struct UserState {
    requests: PathBuf,
    responses: PathBuf,
    request_uid: u32,
    request_gid: Option<u32>,
    request_mode: u32,
    response_uid: u32,
    response_gid: Option<u32>,
    response_mode: u32,
}

impl UserState {
    pub fn requests_dir(&self) -> &Path {
        &self.requests
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn user_identity(user: &str) -> Result<UserIdentity> {
    if !valid_name(user) {
        bail!("invalid CUPS user name {user:?}");
    }
    let name = CString::new(user).expect("validated user name contains no NUL");
    let entry = unsafe { libc::getpwnam(name.as_ptr()) };
    if entry.is_null() {
        bail!("CUPS user {user:?} is not a local account");
    }
    Ok(UserIdentity {
        uid: unsafe { (*entry).pw_uid },
        gid: unsafe { (*entry).pw_gid },
    })
}

fn account_identity(name: &str) -> Result<(u32, u32)> {
    let name = CString::new(name).expect("literal contains no NUL");
    let entry = unsafe { libc::getpwnam(name.as_ptr()) };
    if entry.is_null() {
        bail!("required account {:?} does not exist", name);
    }
    Ok((unsafe { (*entry).pw_uid }, unsafe { (*entry).pw_gid }))
}

/// Resolve the account cupsd runs backends as.
///
/// An explicit `paths.backend_user` wins.  Otherwise the configured accounts are
/// tried in order and the first one that exists locally is used.  Debian/Ubuntu
/// and upstream CUPS have `lp`; Arch Linux and Fedora only ship `cups`.
pub fn backend_account(configured: &str) -> Result<(String, u32)> {
    if !configured.is_empty() {
        return Ok((configured.to_string(), account_identity(configured)?.0));
    }
    for candidate in BACKEND_ACCOUNTS {
        if let Ok((uid, _)) = account_identity(candidate) {
            return Ok((candidate.to_string(), uid));
        }
    }
    bail!(
        "cannot find the account cupsd runs backends as (tried {BACKEND_ACCOUNTS:?}); \
         set paths.backend_user in the configuration"
    )
}

fn bridge_gid(user: &str) -> Result<u32> {
    let name = CString::new(format!("dupriman-{user}"))?;
    let entry = unsafe { libc::getgrnam(name.as_ptr()) };
    if entry.is_null() {
        bail!("required bridge group {:?} does not exist", name);
    }
    Ok(unsafe { (*entry).gr_gid })
}

fn euid() -> u32 {
    unsafe { libc::geteuid() }
}

fn mode_is(metadata: &fs::Metadata, mode: u32) -> bool {
    metadata.permissions().mode() & 0o7777 == mode
}

fn secure_state_root(root: &Path, identity: UserIdentity) -> Result<()> {
    let metadata = fs::metadata(root)
        .with_context(|| format!("cannot inspect state directory {}", root.display()))?;
    if !metadata.is_dir() {
        bail!("state directory {} is not a directory", root.display());
    }
    if metadata.permissions().mode() & 0o022 != 0 {
        bail!(
            "state directory {} must not be group/world writable",
            root.display()
        );
    }
    let valid_owner = if euid() == 0 {
        metadata.uid() == 0
    } else {
        metadata.uid() == 0 || metadata.uid() == identity.uid
    };
    if !valid_owner {
        bail!(
            "state directory {} must be owned by root or the submitting user",
            root.display(),
        );
    }
    Ok(())
}

fn chown(path: &Path, uid: u32, gid: u32) -> Result<()> {
    let path =
        CString::new(path.as_os_str().as_encoded_bytes()).expect("filesystem path contains no NUL");
    if unsafe { libc::chown(path.as_ptr(), uid, gid) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("cannot assign ownership of {}", path.to_string_lossy()));
    }
    Ok(())
}

fn secure_dir(path: &Path, uid: u32, gid: u32, mode: u32) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("cannot inspect {}", path.display()))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != uid
        || metadata.gid() != gid
        || !mode_is(&metadata, mode)
    {
        bail!("insecure prompt directory {}", path.display());
    }
    Ok(())
}

fn provision_dir(path: &Path, uid: u32, gid: u32, mode: u32) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => {
            chown(path, uid, gid)?;
            fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(error).with_context(|| format!("cannot create {}", path.display()));
        }
    }
    secure_dir(path, uid, gid, mode)
}

fn direct_state(root: &Path, user: &str, identity: UserIdentity) -> Result<UserState> {
    let dir = root.join(user);
    match fs::symlink_metadata(&dir) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                || !metadata.is_dir()
                || metadata.uid() != identity.uid
                || !mode_is(&metadata, 0o700)
            {
                bail!(
                    "prompt directory {} must be private to {user}",
                    dir.display()
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && euid() == identity.uid => {
            fs::create_dir(&dir)
                .with_context(|| format!("cannot create prompt directory {}", dir.display()))?;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        }
        Err(error) => {
            return Err(error).with_context(|| format!("cannot inspect {}", dir.display()));
        }
    }
    Ok(UserState {
        requests: dir.clone(),
        responses: dir,
        request_uid: identity.uid,
        request_gid: None,
        request_mode: 0o600,
        response_uid: identity.uid,
        response_gid: None,
        response_mode: 0o600,
    })
}

fn bridge_state(
    root: &Path,
    user: &str,
    identity: UserIdentity,
    configured_backend_user: &str,
) -> Result<UserState> {
    let bridge_gid = bridge_gid(user)?;
    let user_dir = root.join(user);
    let requests = user_dir.join("requests");
    let responses = user_dir.join("responses");

    // An existing requests directory names the backend account outright: its
    // owner is whoever cupsd runs backends as on this system.
    let backend_uid = match fs::symlink_metadata(&requests) {
        Ok(metadata) => {
            let uid = metadata.uid();
            if uid == identity.uid {
                bail!(
                    "requests directory {} must not be owned by the submitting user",
                    requests.display()
                );
            }
            uid
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && euid() == 0 => {
            backend_account(configured_backend_user)?.1
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => bail!(
            "prompt state for {user:?} is not provisioned; root must create the bridge layout"
        ),
        Err(error) => {
            return Err(error).with_context(|| format!("cannot inspect {}", requests.display()));
        }
    };

    if euid() == 0 {
        provision_dir(&user_dir, 0, 0, 0o711)?;
        provision_dir(&requests, backend_uid, bridge_gid, 0o2750)?;
        provision_dir(&responses, identity.uid, bridge_gid, 0o2770)?;
    } else {
        secure_dir(&user_dir, 0, 0, 0o711)?;
        secure_dir(&requests, backend_uid, bridge_gid, 0o2750)?;
        secure_dir(&responses, identity.uid, bridge_gid, 0o2770)?;
    }
    Ok(UserState {
        requests,
        responses,
        request_uid: backend_uid,
        request_gid: Some(bridge_gid),
        request_mode: 0o640,
        response_uid: identity.uid,
        response_gid: Some(bridge_gid),
        response_mode: 0o640,
    })
}

pub fn user_state(root: &Path, user: &str, configured_backend_user: &str) -> Result<UserState> {
    let identity = user_identity(user)?;
    secure_state_root(root, identity)?;
    let dir = root.join(user);
    match fs::symlink_metadata(&dir) {
        Ok(metadata) if metadata.uid() == 0 => {
            bridge_state(root, user, identity, configured_backend_user)
        }
        Ok(metadata) if metadata.uid() == identity.uid => direct_state(root, user, identity),
        Ok(_) => bail!("prompt directory {} has an unexpected owner", dir.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && euid() == 0 => {
            bridge_state(root, user, identity, configured_backend_user)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && euid() == identity.uid => {
            direct_state(root, user, identity)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => bail!(
            "prompt state for {user:?} is not provisioned; root must create the bridge layout"
        ),
        Err(error) => Err(error).with_context(|| format!("cannot inspect {}", dir.display())),
    }
}

pub fn request_path(state: &UserState, id: &str) -> PathBuf {
    state.requests.join(format!("req-{id}.json"))
}

pub fn response_path(state: &UserState, id: &str) -> PathBuf {
    state.responses.join(format!("resp-{id}.json"))
}

pub fn random_token() -> Result<String> {
    let mut bytes = [0_u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub fn make_request_id(job_id: &str) -> Result<String> {
    Ok(format!("{job_id}-{}", &random_token()?[..16]))
}

fn secure_file(path: &Path, uid: u32, gid: Option<u32>, mode: u32) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("cannot inspect {}", path.display()))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != uid
        || gid.is_some_and(|gid| metadata.gid() != gid)
        || !mode_is(&metadata, mode)
    {
        bail!("rejected insecure prompt file {}", path.display());
    }
    Ok(())
}

/// Remove request files whose backend is gone or that are long past any
/// possible answer.  Best effort: a failure here must never block a print.
fn sweep_stale(state: &UserState) {
    let Ok(entries) = fs::read_dir(state.requests_dir()) else {
        return;
    };
    for path in entries.filter_map(|entry| entry.ok()).map(|e| e.path()) {
        let is_request = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("req-") && name.ends_with(".json"));
        if !is_request {
            continue;
        }
        // Requests written by older versions carry no pid, so fall back to a
        // generous age bound for those.
        let stale = is_stale(&path, Some(Duration::from_secs(24 * 60 * 60)))
            || fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<PromptRequest>(&bytes).ok())
                .is_some_and(|request| !request.is_live());
        if stale {
            fs::remove_file(&path).ok();
        }
    }
}

/// Send a private request and return the recipient's state and identity.
pub fn send_request(
    root: &Path,
    req: &PromptRequest,
    configured_backend_user: &str,
) -> Result<(UserState, UserIdentity)> {
    let identity = user_identity(&req.user)?;
    let state = user_state(root, &req.user, configured_backend_user)?;
    if euid() != 0 && euid() != state.request_uid {
        bail!(
            "backend user cannot write prompt requests for {:?}",
            req.user
        );
    }
    // The desktop user can read `requests/` but not unlink in it, so only the
    // backend can clear files left by jobs CUPS killed.  Do it while holding
    // write access, before adding a new one.
    sweep_stale(&state);
    let path = request_path(&state, &req.id);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(state.request_mode)
        .open(&path)
        .with_context(|| format!("cannot create request {}", path.display()))?;
    file.write_all(&serde_json::to_vec_pretty(req)?)?;
    file.sync_all()?;
    drop(file);
    if euid() == 0 {
        chown(
            &path,
            state.request_uid,
            state.request_gid.unwrap_or(identity.gid),
        )?;
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(state.request_mode))?;
    secure_file(
        &path,
        state.request_uid,
        state.request_gid,
        state.request_mode,
    )?;
    Ok((state, identity))
}

pub fn wait_response(
    state: &UserState,
    id: &str,
    capability: &str,
    identity: UserIdentity,
    timeout: Option<Duration>,
    poll: Duration,
) -> Result<WaitOutcome> {
    let start = Instant::now();
    let path = response_path(state, id);
    loop {
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                secure_file(&path, identity.uid, state.response_gid, state.response_mode)?;
                let response: PromptResponse = serde_json::from_slice(&fs::read(&path)?)
                    .context("cannot parse prompt response")?;
                if response.id != id || response.capability != capability {
                    bail!("rejected prompt response with an invalid capability");
                }
                cleanup(state, id);
                return Ok(if response.confirmed {
                    WaitOutcome::Confirmed
                } else {
                    WaitOutcome::Declined(response.reason)
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("cannot inspect {}", path.display()));
            }
        }
        if let Some(timeout) = timeout
            && start.elapsed() >= timeout
        {
            cleanup(state, id);
            return Ok(WaitOutcome::Timeout);
        }
        sleep(poll);
    }
}

pub fn cleanup(state: &UserState, id: &str) {
    fs::remove_file(request_path(state, id)).ok();
    fs::remove_file(response_path(state, id)).ok();
}

/// List the requests awaiting a decision.
///
/// Returns an error when the directory cannot be read at all.  That failure is
/// the difference between "nothing to ask" and "the daemon cannot see the
/// bridge", and silently treating the two alike leaves a job stalled forever
/// with no visible reason.  Requests whose backend has already exited are
/// skipped: the answer would go nowhere.
pub fn pending_requests(
    state: &UserState,
    max_age: Option<Duration>,
) -> Result<Vec<(PathBuf, PromptRequest)>> {
    let entries = fs::read_dir(state.requests_dir())
        .with_context(|| format!("cannot read {}", state.requests_dir().display()))?;
    let mut items: Vec<(PathBuf, PromptRequest)> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("req-") && name.ends_with(".json"))
        })
        .filter(|path| {
            secure_file(
                path,
                state.request_uid,
                state.request_gid,
                state.request_mode,
            )
            .is_ok()
        })
        .filter(|path| !is_stale(path, max_age))
        .filter_map(|path| {
            let request = serde_json::from_slice::<PromptRequest>(&fs::read(&path).ok()?).ok()?;
            request.is_live().then_some((path, request))
        })
        .collect();
    items.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(items)
}

/// Resolve a group name to its gid, if it exists.
pub fn group_id(name: &str) -> Option<u32> {
    let name = CString::new(name).ok()?;
    let entry = unsafe { libc::getgrnam(name.as_ptr()) };
    if entry.is_null() {
        None
    } else {
        Some(unsafe { (*entry).gr_gid })
    }
}

pub fn send_response(state: &UserState, response: &PromptResponse) -> Result<()> {
    if euid() != state.response_uid {
        bail!("only the submitting user may write a prompt response");
    }
    let path = response_path(state, &response.id);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(state.response_mode)
        .open(&path)
        .with_context(|| format!("cannot create response {}", path.display()))?;
    file.write_all(&serde_json::to_vec_pretty(response)?)?;
    file.sync_all()?;
    drop(file);
    fs::set_permissions(&path, fs::Permissions::from_mode(state.response_mode))?;
    secure_file(
        &path,
        state.response_uid,
        state.response_gid,
        state.response_mode,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current_user() -> String {
        std::env::var("USER").expect("tests require USER")
    }

    /// Regression: the backend account must be detected, never hardcoded to
    /// `lp`.  Arch Linux ships only `cups`, so `account_identity("lp")` used to
    /// abort every duplex job there ("required account \"lp\" does not exist").
    #[test]
    fn backend_account_is_detected_not_assumed() {
        // At least one of the configured candidates must exist on any host that
        // actually runs CUPS; on hosts without either, an explicit override wins.
        let detected = backend_account("");
        match detected {
            Ok((name, uid)) => {
                assert!(BACKEND_ACCOUNTS.contains(&name.as_str()));
                assert_ne!(uid, 0, "backend account must not be root");
            }
            Err(_) => {
                // Neither lp nor cups installed: an explicit name still resolves,
                // and an unknown name fails with a diagnosable message.
                assert!(backend_account("definitely-not-a-user").is_err());
            }
        }
        // An explicit configuration value bypasses detection entirely.
        let me = current_user();
        assert_eq!(backend_account(&me).unwrap().0, me);
    }

    /// Regression: an existing `requests/` directory is authoritative for the
    /// backend uid, so a layout provisioned for `cups` works even where `lp`
    /// does not exist.
    #[test]
    fn existing_requests_dir_names_the_backend_account() {
        let root =
            std::env::temp_dir().join(format!("dupriman-proto-owner-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        fs::create_dir(&root).unwrap();
        let user = current_user();
        let requests = root.join(&user).join("requests");
        fs::create_dir_all(&requests).unwrap();
        // Owned by us, so bridge_state rejects it rather than trusting it.
        let identity = user_identity(&user).unwrap();
        assert!(bridge_state(&root, &user, identity, "").is_err());
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn request_response_roundtrip_is_private() {
        let root = std::env::temp_dir().join(format!("dupriman-proto-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        fs::create_dir(&root).unwrap();
        let user = current_user();
        let request = PromptRequest {
            id: "t1".into(),
            capability: random_token().unwrap(),
            job_id: "42".into(),
            user,
            title: "test document".into(),
            detail: "10 pages".into(),
            pid: std::process::id(),
        };
        let (state, identity) = send_request(&root, &request, "").unwrap();
        assert_eq!(
            fs::metadata(state.requests_dir())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(request_path(&state, &request.id))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(pending_requests(&state, None).unwrap().len(), 1);
        send_response(
            &state,
            &PromptResponse {
                id: request.id.clone(),
                capability: request.capability.clone(),
                confirmed: false,
                reason: "cancelled".into(),
            },
        )
        .unwrap();
        let outcome = wait_response(
            &state,
            &request.id,
            &request.capability,
            identity,
            Some(Duration::from_secs(1)),
            Duration::from_millis(10),
        )
        .unwrap();
        assert_eq!(outcome, WaitOutcome::Declined("cancelled".into()));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn mismatched_capability_is_rejected() {
        let root = std::env::temp_dir().join(format!("dupriman-proto-cap-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        fs::create_dir(&root).unwrap();
        let request = PromptRequest {
            id: "t2".into(),
            capability: random_token().unwrap(),
            job_id: "43".into(),
            user: current_user(),
            title: "test document".into(),
            detail: String::new(),
            pid: std::process::id(),
        };
        let (state, identity) = send_request(&root, &request, "").unwrap();
        send_response(
            &state,
            &PromptResponse {
                id: request.id.clone(),
                capability: "wrong".into(),
                confirmed: true,
                reason: String::new(),
            },
        )
        .unwrap();
        assert!(
            wait_response(
                &state,
                &request.id,
                &request.capability,
                identity,
                Some(Duration::from_secs(1)),
                Duration::from_millis(10)
            )
            .is_err()
        );
        fs::remove_dir_all(&root).ok();
    }

    /// CUPS can kill a backend without letting it clean up, and the desktop
    /// user cannot unlink files in `requests/` (the group has r-x, not rwx).  A
    /// stale request must therefore be ignored rather than re-prompted.
    #[test]
    fn requests_from_dead_backends_are_ignored() {
        let request = PromptRequest {
            id: "dead".into(),
            capability: random_token().unwrap(),
            job_id: "7".into(),
            user: current_user(),
            title: "killed job".into(),
            detail: String::new(),
            // 0 means "unknown" and must never be filtered.
            pid: 0,
        };
        assert!(request.is_live());

        let mut gone = request.clone();
        gone.pid = 4_000_000;
        assert!(!gone.is_live(), "a dead backend must not keep prompting");

        let mut alive = request;
        alive.pid = std::process::id();
        assert!(alive.is_live());
    }

    /// A request written by an older version has no pid, so age is the only
    /// signal that the job is gone.
    #[test]
    fn stale_requests_are_dropped_by_age() {
        let root = std::env::temp_dir().join(format!("dupriman-stale-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        fs::create_dir(&root).unwrap();
        let request = PromptRequest {
            id: "t3".into(),
            capability: random_token().unwrap(),
            job_id: "44".into(),
            user: current_user(),
            title: "old job".into(),
            detail: String::new(),
            pid: 0,
        };
        let (state, _) = send_request(&root, &request, "").unwrap();

        // A generous age bound still reports the live request.
        assert_eq!(
            pending_requests(&state, Some(Duration::from_secs(3600)))
                .unwrap()
                .len(),
            1
        );
        // A zero bound treats every existing file as stale.
        assert!(
            pending_requests(&state, Some(Duration::ZERO))
                .unwrap()
                .is_empty()
        );
        // Without a bound nothing is filtered out.
        assert_eq!(pending_requests(&state, None).unwrap().len(), 1);
        fs::remove_dir_all(&root).ok();
    }
}
