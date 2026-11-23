use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use datatab_core::DbError;
use datatab_core::FormFieldKind;
use datatab_core::auth::{
    AuthFormDef, AuthProfile, AuthProviderCapabilities, AuthSession, AuthSessionState,
    DynAuthProvider, FetchOptionsError, FetchOptionsRequest, FetchOptionsResponse,
    ResolvedCredentials, UrlCallback,
};
use interprocess::local_socket::{Stream as IpcStream, prelude::*};

use crate::audit::{ExternalAuditEmitter, ExternalAuditSource};
use crate::auth::{AUTH_PROVIDER_RPC_AUTH_TOKEN_ENV, process_auth_token};
use crate::auth_provider_protocol::{
    AuthProviderHelloRequest, AuthProviderRequestBody, AuthProviderRequestEnvelope,
    AuthProviderResponseBody, AuthProviderResponseEnvelope, FetchFieldOptionsError,
    FetchFieldOptionsRequest, LoginRequest, ResolveCredentialsRequest, ValidateSessionRequest,
};
use crate::envelope::{AUTH_PROVIDER_RPC_API_CONTRACT, AUTH_PROVIDER_RPC_V1_2, ProtocolVersion};
use crate::framing;
use crate::socket::auth_provider_socket_name;
use crate::{RpcApiFamily, auth_provider_rpc_supported_versions, negotiate_highest_mutual_version};

const DEFAULT_STARTUP_TIMEOUT_MS: u64 = 5_000;
const MIN_STARTUP_TIMEOUT_MS: u64 = 1;

/// How long a request other than `Login` may take before the caller gets an error.
const REQUEST_DEADLINE: Duration = Duration::from_secs(60);

/// How long the startup `Hello` may take once the service is running. The service's own
/// startup timeout is added when DataTab launches it, so a slow start keeps its budget.
const PROBE_DEADLINE: Duration = Duration::from_secs(5);

/// How long a `Login` may take: the provider answers only after the user finished the
/// browser step. It matches `SSO_LOGIN_TIMEOUT` in `datatab_ui`'s login modal
/// (`crates/datatab_ui/src/ui/overlays/login_modal.rs`) and `LOGIN_BUDGET` in
/// `datatab_mcp_server` (`src/server/connect_deadline/mod.rs`); `datatab_ipc` cannot depend on
/// either crate, so keep the three equal by hand.
const LOGIN_DEADLINE: Duration = Duration::from_secs(300);

fn probe_deadline(launch: Option<&IpcServiceLaunchConfig>) -> Duration {
    PROBE_DEADLINE + launch.map_or(Duration::ZERO, |config| config.startup_timeout)
}

#[allow(clippy::result_large_err)]
fn receive_probe_result(
    receiver: std::sync::mpsc::Receiver<Result<NormalizedAuthProviderHelloResponse, DbError>>,
    deadline: Duration,
    socket_id: &str,
) -> Result<NormalizedAuthProviderHelloResponse, DbError> {
    match receiver.recv_timeout(deadline) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(DbError::connection_failed(
            format!("auth-provider service '{socket_id}' did not answer Hello within {deadline:?}"),
        )),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(DbError::connection_failed(
            format!("auth-provider service '{socket_id}' probe worker ended without a result"),
        )),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IpcServiceLaunchConfig {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub startup_timeout: Duration,
}

pub struct RpcAuthProvider {
    socket_id: String,
    provider_id: String,
    display_name: String,
    form_definition: AuthFormDef,
    capabilities: AuthProviderCapabilities,
    selected_version: ProtocolVersion,
    /// Whether the provider has opted in to receiving `Password`-kind field
    /// values in `FetchFieldOptions` requests (v1.2+ only).
    secret_dependency_opt_in: bool,
    /// Whether the provider will emit `EmitAuditEvent` intermediate frames (v1.3+ only).
    audit_emit_opt_in: bool,
    /// Sanitizing sink for audit frames emitted by this provider.
    audit_emitter: Option<Arc<dyn ExternalAuditEmitter>>,
    launch: Option<IpcServiceLaunchConfig>,
    request_deadline: Duration,
    login_deadline: Duration,
}

#[derive(Debug)]
struct NormalizedAuthProviderHelloResponse {
    provider_id: String,
    display_name: String,
    form_definition: AuthFormDef,
    capabilities: AuthProviderCapabilities,
    selected_version: ProtocolVersion,
    secret_dependency_opt_in: bool,
    /// Whether the provider opted in to emitting audit events (v1.3+).
    audit_emit_opt_in: bool,
}

/// What one request needs from its `RpcAuthProvider`. It is owned, so the
/// request can run on its own thread while the caller's future waits.
struct RequestTransport {
    socket_id: String,
    provider_id: String,
    selected_version: ProtocolVersion,
    audit_emit_opt_in: bool,
    audit_emitter: Option<Arc<dyn ExternalAuditEmitter>>,
    launch: Option<IpcServiceLaunchConfig>,
    /// Set when the caller gave up (the deadline won or the future was dropped).
    /// The worker then runs no callback and emits no audit event.
    abandoned: Arc<AtomicBool>,
}

impl RpcAuthProvider {
    #[allow(clippy::result_large_err)]
    pub fn build_launch_config(
        socket_id: &str,
        command: Option<&str>,
        args: &[String],
        env: &HashMap<String, String>,
        startup_timeout_ms: Option<u64>,
    ) -> Result<Option<IpcServiceLaunchConfig>, DbError> {
        validate_socket_id(socket_id)?;

        let program = match command.map(str::trim).filter(|value| !value.is_empty()) {
            Some(program) => Some(program.to_string()),
            None if args.is_empty() => None,
            None => {
                return Err(DbError::connection_failed(format!(
                    "Managed auth-provider service '{}' must set an explicit command; default driver-host launch is driver-only",
                    socket_id
                )));
            }
        };

        let Some(program) = program else {
            return Ok(None);
        };

        let startup_timeout_ms = match startup_timeout_ms {
            Some(0) => {
                return Err(DbError::connection_failed(format!(
                    "Startup timeout for service '{}' must be at least {} ms",
                    socket_id, MIN_STARTUP_TIMEOUT_MS
                )));
            }
            Some(timeout) => timeout,
            None => DEFAULT_STARTUP_TIMEOUT_MS,
        };

        let mut env_pairs = env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Vec<_>>();
        env_pairs.sort_by(|left, right| left.0.cmp(&right.0));

        Ok(Some(IpcServiceLaunchConfig {
            program,
            args: args.to_vec(),
            env: env_pairs,
            startup_timeout: Duration::from_millis(startup_timeout_ms),
        }))
    }

    /// Probes services concurrently and returns results in input order.
    pub fn probe_many(
        services: &[(String, Option<IpcServiceLaunchConfig>)],
    ) -> Vec<Result<Self, Box<DbError>>> {
        Self::probe_many_with(services, |socket_id, launch| {
            Self::probe(socket_id, launch.clone()).map_err(Box::new)
        })
    }

    fn probe_many_with<F>(
        services: &[(String, Option<IpcServiceLaunchConfig>)],
        probe: F,
    ) -> Vec<Result<Self, Box<DbError>>>
    where
        F: Fn(&str, &Option<IpcServiceLaunchConfig>) -> Result<Self, Box<DbError>> + Sync,
    {
        thread::scope(|scope| {
            let workers: Vec<_> = services
                .iter()
                .map(|(socket_id, launch)| {
                    let probe = &probe;
                    thread::Builder::new().spawn_scoped(scope, move || probe(socket_id, launch))
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| match worker {
                    Ok(worker) => worker.join().unwrap_or_else(|_| {
                        Err(Box::new(DbError::connection_failed(
                            "Auth-provider probe worker panicked",
                        )))
                    }),
                    Err(error) => Err(Box::new(DbError::connection_failed(format!(
                        "Could not start auth-provider probe: {error}"
                    )))),
                })
                .collect()
        })
    }

    #[allow(clippy::result_large_err)]
    pub fn probe(socket_id: &str, launch: Option<IpcServiceLaunchConfig>) -> Result<Self, DbError> {
        let deadline = probe_deadline(launch.as_ref());
        Self::probe_within(socket_id, launch, deadline)
    }

    /// Like `probe`, but gives up after `deadline`. The `Hello` runs on its own thread, so a
    /// provider that accepts the connection and never answers cannot hold the caller. The
    /// thread ends when the provider answers or closes the socket. A provider that never
    /// does either keeps one worker thread and socket until the app quits, once per service.
    #[allow(clippy::result_large_err)]
    fn probe_within(
        socket_id: &str,
        launch: Option<IpcServiceLaunchConfig>,
        deadline: Duration,
    ) -> Result<Self, DbError> {
        validate_socket_id(socket_id)?;

        let (sender, receiver) = std::sync::mpsc::channel();
        let worker_socket_id = socket_id.to_string();
        let worker_launch = launch.clone();
        thread::Builder::new()
            .name(format!("datatab-auth-provider-probe-{socket_id}"))
            .spawn(move || {
                let hello = Self::perform_hello(&worker_socket_id, worker_launch.as_ref());
                if sender.send(hello).is_err() {
                    log::debug!("Auth-provider probe on '{worker_socket_id}' was abandoned");
                }
            })
            .map_err(|error| {
                DbError::connection_failed(format!(
                    "Could not start the auth-provider probe worker: {error}"
                ))
            })?;

        let hello = receive_probe_result(receiver, deadline, socket_id)?;

        Ok(Self {
            socket_id: socket_id.to_string(),
            provider_id: hello.provider_id,
            display_name: hello.display_name,
            form_definition: hello.form_definition,
            capabilities: hello.capabilities,
            selected_version: hello.selected_version,
            secret_dependency_opt_in: hello.secret_dependency_opt_in,
            audit_emit_opt_in: hello.audit_emit_opt_in,
            audit_emitter: None,
            launch,
            request_deadline: REQUEST_DEADLINE,
            login_deadline: LOGIN_DEADLINE,
        })
    }

    /// Attaches an audit emitter for routing `EmitAuditEvent` intermediate frames.
    ///
    /// Must be called after `probe` and before any `DynAuthProvider` method is invoked.
    /// Has no effect unless the provider set `audit_emit_opt_in=true` in its hello.
    pub fn with_audit_emitter(mut self, emitter: Arc<dyn ExternalAuditEmitter>) -> Self {
        self.audit_emitter = Some(emitter);
        self
    }

    /// Constructs a minimal provider for testing the dispatch loop without a real socket.
    #[cfg(test)]
    fn new_for_test(
        socket_id: &str,
        provider_id: &str,
        audit_emit_opt_in: bool,
        audit_emitter: Option<Arc<dyn ExternalAuditEmitter>>,
    ) -> Self {
        use crate::envelope::AUTH_PROVIDER_RPC_VERSION;
        use datatab_core::auth::AuthProviderCapabilities;

        Self {
            socket_id: socket_id.to_string(),
            provider_id: provider_id.to_string(),
            display_name: "Test Provider".to_string(),
            form_definition: datatab_core::auth::AuthFormDef { tabs: vec![] },
            capabilities: AuthProviderCapabilities::default(),
            selected_version: AUTH_PROVIDER_RPC_VERSION,
            secret_dependency_opt_in: false,
            audit_emit_opt_in,
            audit_emitter,
            launch: None,
            request_deadline: REQUEST_DEADLINE,
            login_deadline: LOGIN_DEADLINE,
        }
    }

    /// Replaces the two deadlines so a test runs in milliseconds.
    #[cfg(test)]
    fn with_deadlines(mut self, request_deadline: Duration, login_deadline: Duration) -> Self {
        self.request_deadline = request_deadline;
        self.login_deadline = login_deadline;
        self
    }

    fn request_transport(&self) -> RequestTransport {
        RequestTransport {
            socket_id: self.socket_id.clone(),
            provider_id: self.provider_id.clone(),
            selected_version: self.selected_version,
            audit_emit_opt_in: self.audit_emit_opt_in,
            audit_emitter: self.audit_emitter.clone(),
            launch: self.launch.clone(),
            abandoned: Arc::new(AtomicBool::new(false)),
        }
    }

    #[allow(clippy::result_large_err)]
    fn perform_hello(
        socket_id: &str,
        launch: Option<&IpcServiceLaunchConfig>,
    ) -> Result<NormalizedAuthProviderHelloResponse, DbError> {
        ensure_host_running_for(socket_id, launch)?;

        let name = auth_provider_socket_name(socket_id)
            .map_err(|error| DbError::connection_failed(error.to_string()))?;

        let mut stream = IpcStream::connect(name)
            .map_err(|error| DbError::connection_failed(error.to_string()))?;

        // The process-global store is the primary source; the caller-supplied
        // environment variable remains the fallback for processes that never
        // initialized the store (e.g. standalone `datatab mcp`).
        let auth_token = process_auth_token()
            .filter(|token| !token.is_empty())
            .map(str::to_string)
            .or_else(|| {
                std::env::var(AUTH_PROVIDER_RPC_AUTH_TOKEN_ENV)
                    .ok()
                    .filter(|token| !token.is_empty())
            });

        let request = AuthProviderRequestEnvelope::new(
            AUTH_PROVIDER_RPC_API_CONTRACT.version,
            0,
            AuthProviderRequestBody::Hello(AuthProviderHelloRequest {
                client_name: "datatab_ipc".to_string(),
                client_version: env!("CARGO_PKG_VERSION").to_string(),
                supported_versions: auth_provider_rpc_supported_versions().to_vec(),
                auth_token,
            }),
        );

        framing::send_msg(&mut stream, &request)?;
        let response: AuthProviderResponseEnvelope = framing::recv_msg(&mut stream)?;

        if response.request_id != request.request_id {
            return Err(DbError::connection_failed(format!(
                "Request ID mismatch: sent {}, got {}",
                request.request_id, response.request_id
            )));
        }

        match response.body {
            AuthProviderResponseBody::Hello(_)
            | AuthProviderResponseBody::HelloV1_1(_)
            | AuthProviderResponseBody::HelloV1_2(_)
            | AuthProviderResponseBody::HelloV1_3(_) => {
                let hello = normalize_hello_response(response.body)?;

                validate_hello_selected_version(
                    hello.selected_version,
                    auth_provider_rpc_supported_versions(),
                )?;

                Ok(hello)
            }
            AuthProviderResponseBody::Error(error) => Err(error.into_db_error()),
            _ => Err(DbError::connection_failed(
                "Unexpected response to auth-provider Hello".to_string(),
            )),
        }
    }

    /// Sends `body` and waits for the provider's answer without blocking the
    /// calling thread (see `run_request_worker`).
    #[allow(clippy::result_large_err)]
    async fn send_request(
        &self,
        body: AuthProviderRequestBody,
    ) -> Result<Vec<AuthProviderResponseEnvelope>, DbError> {
        let transport = self.request_transport();

        run_request_worker(
            &self.socket_id,
            &self.provider_id,
            self.request_deadline,
            Arc::clone(&transport.abandoned),
            move || transport.send_request(body, &mut |_| {}),
        )
        .await
    }
}

impl RequestTransport {
    #[allow(clippy::result_large_err)]
    fn connect_stream(&self) -> Result<IpcStream, DbError> {
        ensure_host_running_for(&self.socket_id, self.launch.as_ref())?;

        let name = auth_provider_socket_name(&self.socket_id)
            .map_err(|error| DbError::connection_failed(error.to_string()))?;

        IpcStream::connect(name).map_err(|error| DbError::connection_failed(error.to_string()))
    }

    /// Connects and runs one request to its terminal frame. Blocking: call it
    /// from a request worker, never from an executor thread.
    #[allow(clippy::result_large_err)]
    fn send_request(
        &self,
        body: AuthProviderRequestBody,
        on_response: &mut dyn FnMut(&AuthProviderResponseBody),
    ) -> Result<Vec<AuthProviderResponseEnvelope>, DbError> {
        let mut stream = self.connect_stream()?;
        self.dispatch_request_loop(&mut stream, body, on_response)
    }

    /// Sends `body` to the provider over `stream` and collects all response frames.
    ///
    /// The loop exits on the first frame with `done = true`. To guard against a misbehaving
    /// provider that sends frames indefinitely, it aborts after `MAX_INTERMEDIATE_FRAMES`
    /// frames without a terminal `done = true` frame.
    ///
    /// The wall-clock limit is not checked here: `run_request_worker` races the whole request
    /// against its deadline, which also covers a provider that sends no frame at all. A
    /// `recv_msg` call that never returns cannot be interrupted from this loop.
    ///
    /// `on_response` sees each collected frame when it arrives, so a caller can act on a
    /// progress frame while the provider still works on the terminal one.
    #[allow(clippy::result_large_err)]
    fn dispatch_request_loop<S: std::io::Read + std::io::Write>(
        &self,
        stream: &mut S,
        body: AuthProviderRequestBody,
        on_response: &mut dyn FnMut(&AuthProviderResponseBody),
    ) -> Result<Vec<AuthProviderResponseEnvelope>, DbError> {
        const MAX_INTERMEDIATE_FRAMES: usize = 1000;

        let request = AuthProviderRequestEnvelope::new(self.selected_version, 1, body);
        let correlation_id = uuid::Uuid::new_v4().to_string();

        framing::send_msg(&mut *stream, &request)?;

        let mut frame_count: usize = 0;
        let mut responses = Vec::new();

        loop {
            let response: AuthProviderResponseEnvelope = framing::recv_msg(&mut *stream)?;

            frame_count += 1;

            if frame_count > MAX_INTERMEDIATE_FRAMES {
                log::warn!(
                    "auth-provider '{}' exceeded {} frames in one request; aborting",
                    self.provider_id,
                    MAX_INTERMEDIATE_FRAMES
                );
                return Err(DbError::connection_failed(format!(
                    "auth-provider '{}' exceeded frame budget ({} intermediate frames)",
                    self.provider_id, MAX_INTERMEDIATE_FRAMES
                )));
            }

            if response.request_id != request.request_id {
                return Err(DbError::connection_failed(format!(
                    "Request ID mismatch: sent {}, got {}",
                    request.request_id, response.request_id
                )));
            }

            let done = response.done;

            match &response.body {
                AuthProviderResponseBody::EmitAuditEvent(dto) if !done => {
                    if self.audit_emit_opt_in
                        && !self.abandoned.load(Ordering::Acquire)
                        && let Some(sink) = &self.audit_emitter
                    {
                        let source = ExternalAuditSource::AuthProvider {
                            socket_id: self.socket_id.clone(),
                            provider_id: self.provider_id.clone(),
                            correlation_id: correlation_id.clone(),
                        };
                        sink.emit(source, dto.clone());
                    }
                    // Never push EmitAuditEvent frames to `responses` — keep it terminal-only.
                }
                _ => {
                    if !self.abandoned.load(Ordering::Acquire) {
                        on_response(&response.body);
                    }
                    responses.push(response);
                }
            }

            if done {
                return Ok(responses);
            }
        }
    }
}

/// Runs one blocking request on its own thread and waits for its result
/// without blocking the caller: the Settings form and the connection manager
/// await these calls on the UI thread, and a login waits there for as long as
/// the user takes in the browser.
///
/// A second thread waits `deadline` for the worker to finish and sends a timeout on the
/// same channel when it does not, so the caller gets an error even when the provider sends
/// no frame at all. The timer ends as soon as the worker does. It is not a tokio timer: the
/// GPUI executors have no tokio runtime.
///
/// A dropped future or a lost race sets `abandoned`, so the worker runs no more callbacks
/// and emits no more audit events. Its thread ends when the provider answers or closes the
/// socket. `interprocess` offers no way to
/// shut its stream down from another thread: the Unix stream has no public
/// `shutdown` (`os/unix/c_wrappers.rs:448` is `pub(super)`) and the Windows
/// stream has no read timeout (`os/windows/named_pipe/local_socket/stream.rs:53`).
async fn run_request_worker<Output, Work>(
    socket_id: &str,
    provider_id: &str,
    deadline: Duration,
    abandoned: Arc<AtomicBool>,
    work: Work,
) -> Result<Output, DbError>
where
    Output: Send + 'static,
    Work: FnOnce() -> Result<Output, DbError> + Send + 'static,
{
    let (sender, mut receiver) = tokio::sync::mpsc::channel::<Option<Result<Output, DbError>>>(2);
    let timer_sender = sender.clone();
    let worker_socket_id = socket_id.to_string();
    let (worker_done, timer_done) = std::sync::mpsc::channel::<()>();
    let _abandon_on_drop = AbandonOnDrop(Arc::clone(&abandoned));

    thread::Builder::new()
        .name(format!("datatab-auth-provider-{socket_id}"))
        .spawn(move || {
            // Dropped when the closure ends, which wakes the timer.
            let _worker_done = worker_done;
            if sender.try_send(Some(work())).is_err() {
                log::debug!("Auth-provider request on '{worker_socket_id}' was abandoned");
            }
        })
        .map_err(|error| {
            DbError::connection_failed(format!(
                "Could not start the auth-provider request worker: {error}"
            ))
        })?;

    thread::Builder::new()
        .name(format!("datatab-auth-provider-deadline-{socket_id}"))
        .spawn(move || run_deadline_timer(&timer_done, deadline, &timer_sender))
        .map_err(|error| {
            DbError::connection_failed(format!(
                "Could not start the auth-provider request deadline: {error}"
            ))
        })?;

    match receiver.recv().await {
        Some(Some(result)) => result,
        Some(None) => {
            abandoned.store(true, Ordering::Release);
            log::warn!("auth-provider '{provider_id}' request exceeded {deadline:?}; giving up");
            Err(DbError::connection_failed(format!(
                "auth-provider '{provider_id}' did not answer within {deadline:?}"
            )))
        }
        None => Err(DbError::connection_failed(
            "Auth-provider request worker stopped without a result".to_string(),
        )),
    }
}

/// Sets the flag when the request future is dropped, finished or not.
struct AbandonOnDrop(Arc<AtomicBool>);

impl Drop for AbandonOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Waits up to `deadline` for the worker's done signal (its sender dropping) and sends the
/// timeout only when the deadline passes first.
fn run_deadline_timer<Output>(
    worker_done: &std::sync::mpsc::Receiver<()>,
    deadline: Duration,
    sender: &tokio::sync::mpsc::Sender<Option<Result<Output, DbError>>>,
) {
    match worker_done.recv_timeout(deadline) {
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            if sender.try_send(None).is_err() {
                log::trace!("Auth-provider request finished before its deadline");
            }
        }
        Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {}
    }
}

impl RpcAuthProvider {
    /// Fetch the available options for a `DynamicSelect` field.
    ///
    /// Returns `FetchFieldOptionsError::SessionExpired` if the provider
    /// response cannot be deserialized or is missing. Password-kind field
    /// values are stripped from `dependencies` and from the profile unless
    /// `secret_dependency_opt_in` is true on this provider AND the target
    /// field lists that field in its `depends_on`. The profile also loses
    /// every field this provider's form does not declare.
    ///
    /// A `session` of `None` or JSON `null` is sent as no session. Any other
    /// value returns `FetchFieldOptionsError::Permanent` without a request.
    pub async fn fetch_dynamic_options(
        &self,
        profile: &AuthProfile,
        field_id: &str,
        raw_dependencies: HashMap<String, String>,
        session: Option<serde_json::Value>,
    ) -> Result<crate::auth_provider_protocol::FetchFieldOptionsResponse, FetchFieldOptionsError>
    {
        // Only v1.2+ providers support FetchDynamicOptions.
        let supports_fetch = self.selected_version.major > AUTH_PROVIDER_RPC_V1_2.major
            || (self.selected_version.major == AUTH_PROVIDER_RPC_V1_2.major
                && self.selected_version.minor >= AUTH_PROVIDER_RPC_V1_2.minor);
        if !supports_fetch {
            return Err(FetchFieldOptionsError::Permanent(format!(
                "auth-provider '{}' does not support FetchDynamicOptions (selected protocol {}.{})",
                self.provider_id, self.selected_version.major, self.selected_version.minor
            )));
        }

        // postcard cannot decode a `serde_json::Value`, so a provider would
        // reject any request that carries one. JSON `null` holds no data and
        // is sent as "no session"; real session data is refused here.
        let session = match session {
            None | Some(serde_json::Value::Null) => None,
            Some(_) => {
                return Err(FetchFieldOptionsError::Permanent(format!(
                    "auth-provider '{}' cannot receive session data for dynamic options",
                    self.provider_id
                )));
            }
        };

        // One filter for the dependency map and the profile: both drop the
        // secret values the provider has not opted in to receiving.
        let secret_filter = FetchSecretFilter::new(
            &self.form_definition,
            field_id,
            self.secret_dependency_opt_in,
        );
        let dependencies = secret_filter.dependencies(&raw_dependencies);

        let profile_json =
            profile_to_fetch_wire_json(profile, &secret_filter).map_err(|error| {
                FetchFieldOptionsError::Permanent(format!("could not serialize profile: {error}"))
            })?;

        let request_body = AuthProviderRequestBody::FetchDynamicOptions(FetchFieldOptionsRequest {
            profile_json,
            field_id: field_id.to_string(),
            dependencies,
            session,
        });

        let responses = self
            .send_request(request_body)
            .await
            .map_err(|error| FetchFieldOptionsError::Transient(error.to_string()))?;

        let last = responses
            .last()
            .ok_or(FetchFieldOptionsError::SessionExpired)?;

        match &last.body {
            AuthProviderResponseBody::DynamicOptions(response) => Ok(response.clone()),
            AuthProviderResponseBody::Error(error) => {
                Err(FetchFieldOptionsError::Permanent(error.message.clone()))
            }
            _ => Err(FetchFieldOptionsError::SessionExpired),
        }
    }
}

/// Build the `dependencies` map to include in a `FetchFieldOptionsRequest`.
///
/// Password-kind field values are excluded unless both conditions hold:
/// 1. `secret_dependency_opt_in` is `true` (provider opted in).
/// 2. The Password field's id is listed in the target field's `depends_on`.
///
/// Exposed as a free function so unit tests can exercise the logic without
/// spinning up an RPC socket.
pub fn build_fetch_dependencies(
    form_def: &AuthFormDef,
    field_id: &str,
    raw_dependencies: &HashMap<String, String>,
    secret_dependency_opt_in: bool,
) -> HashMap<String, String> {
    FetchSecretFilter::new(form_def, field_id, secret_dependency_opt_in)
        .dependencies(raw_dependencies)
}

/// Which secret values may go into a `FetchDynamicOptions` request for one
/// field.
///
/// A `Password` or `WriteOnly` value is released only when the provider opted
/// in via `secret_dependency_opt_in` AND the target field lists that field in
/// its `depends_on`. The dependency map and the profile follow this one rule.
/// The profile is also cut down to the fields the form declares.
struct FetchSecretFilter<'a> {
    /// Ids of every field of the form.
    declared_field_ids: HashSet<&'a str>,
    /// Ids of the `Password` and `WriteOnly` fields of the form.
    secret_field_ids: HashSet<&'a str>,
    /// The target field's `depends_on`, when the provider opted in.
    released_field_ids: HashSet<&'a str>,
}

impl<'a> FetchSecretFilter<'a> {
    fn new(form_def: &'a AuthFormDef, field_id: &str, secret_dependency_opt_in: bool) -> Self {
        let fields = || {
            form_def
                .tabs
                .iter()
                .flat_map(|tab| tab.sections.iter())
                .flat_map(|section| section.fields.iter())
        };

        let declared_field_ids = fields().map(|field| field.id.as_str()).collect();

        let secret_field_ids = fields()
            .filter(|field| {
                field.kind == FormFieldKind::Password || field.kind == FormFieldKind::WriteOnly
            })
            .map(|field| field.id.as_str())
            .collect();

        let released_field_ids = if secret_dependency_opt_in {
            fields()
                .find(|field| field.id == field_id)
                .and_then(|field| {
                    if let FormFieldKind::DynamicSelect { depends_on, .. } = &field.kind {
                        Some(depends_on.iter().map(String::as_str).collect())
                    } else {
                        None
                    }
                })
                .unwrap_or_default()
        } else {
            HashSet::new()
        };

        Self {
            declared_field_ids,
            secret_field_ids,
            released_field_ids,
        }
    }

    /// Whether the value of form field `field_id` must stay out of the request.
    fn withholds(&self, field_id: &str) -> bool {
        self.secret_field_ids.contains(field_id) && !self.released_field_ids.contains(field_id)
    }

    /// Whether an entry of `AuthProfile::fields` may be sent. The form must
    /// declare the field: a profile can carry other keys, such as the fields
    /// `expand_auth_profile_refs` copies in from a referenced profile, and
    /// nothing says those hold no secret.
    fn sends_profile_field(&self, field_id: &str) -> bool {
        self.declared_field_ids.contains(field_id) && !self.withholds(field_id)
    }

    /// Whether an entry of `AuthProfile::secret_fields` may be sent. Such a
    /// value is secret whatever kind the form gives its field now.
    fn sends_stored_secret(&self, field_id: &str) -> bool {
        self.declared_field_ids.contains(field_id) && self.released_field_ids.contains(field_id)
    }

    fn dependencies(&self, raw_dependencies: &HashMap<String, String>) -> HashMap<String, String> {
        raw_dependencies
            .iter()
            .filter(|(dependency_field_id, _)| !self.withholds(dependency_field_id))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }
}

/// Compute a stable hash of the dependencies map for cache keying.
///
/// The hash is derived from the sorted (key, value) pairs so it is
/// deterministic regardless of HashMap iteration order.
pub fn hash_dependencies(dependencies: &HashMap<String, String>) -> u64 {
    let mut pairs: Vec<(&str, &str)> = dependencies
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    pairs.sort_unstable();

    let mut hasher = DefaultHasher::new();
    for (key, value) in pairs {
        key.hash(&mut hasher);
        value.hash(&mut hasher);
    }
    hasher.finish()
}

/// Serialize an auth profile for the IPC wire, re-merging secret-kind field
/// values back into the flat `fields` map the external provider expects.
///
/// This and `profile_to_fetch_wire_json` are the only places where
/// `AuthProfile::secret_fields` are exposed in plaintext. The derived
/// `Serialize` skips that map, so without this merge
/// an external provider would never receive the secret values it needs to
/// authenticate. The output shape is identical to the legacy
/// `serde_json::to_string(profile)` that kept secrets inline in `fields`.
fn profile_to_wire_json(profile: &AuthProfile) -> Result<String, serde_json::Error> {
    use secrecy::ExposeSecret;

    let mut value = serde_json::to_value(profile)?;

    if let Some(fields) = value.get_mut("fields").and_then(|f| f.as_object_mut()) {
        for (key, secret) in &profile.secret_fields {
            fields.insert(
                key.clone(),
                serde_json::Value::String(secret.expose_secret().to_string()),
            );
        }
    }

    serde_json::to_string(&value)
}

/// Serialize an auth profile for a `FetchDynamicOptions` request.
///
/// Same shape as `profile_to_wire_json`, but `fields` is an allowlist: only
/// the fields the provider's form declares, and a secret one only under the
/// rule of the dependency map. The provider reads this profile to list
/// options, not to authenticate.
fn profile_to_fetch_wire_json(
    profile: &AuthProfile,
    filter: &FetchSecretFilter<'_>,
) -> Result<String, serde_json::Error> {
    use secrecy::ExposeSecret;

    let mut value = serde_json::to_value(profile)?;

    if let Some(fields) = value.get_mut("fields").and_then(|f| f.as_object_mut()) {
        // The Settings form snapshot keeps secret inputs inline in `fields`.
        fields.retain(|key, _| filter.sends_profile_field(key));

        for (key, secret) in &profile.secret_fields {
            if filter.sends_stored_secret(key) {
                fields.insert(
                    key.clone(),
                    serde_json::Value::String(secret.expose_secret().to_string()),
                );
            }
        }
    }

    serde_json::to_string(&value)
}

#[async_trait::async_trait]
impl DynAuthProvider for RpcAuthProvider {
    fn provider_id(&self) -> &str {
        &self.provider_id
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }

    fn form_def(&self) -> &AuthFormDef {
        &self.form_definition
    }

    fn capabilities(&self) -> &AuthProviderCapabilities {
        &self.capabilities
    }

    async fn validate_session(&self, profile: &AuthProfile) -> Result<AuthSessionState, DbError> {
        let profile_json = profile_to_wire_json(profile)
            .map_err(|error| DbError::QueryFailed(error.to_string().into()))?;

        let responses = self
            .send_request(AuthProviderRequestBody::ValidateSession(
                ValidateSessionRequest { profile_json },
            ))
            .await?;

        let response = responses.last().ok_or_else(|| {
            DbError::connection_failed("Auth-provider returned no response".to_string())
        })?;

        match &response.body {
            AuthProviderResponseBody::SessionState { state } => Ok(state.clone().into()),
            AuthProviderResponseBody::Error(error) => Err(error.clone().into_db_error()),
            _ => Err(DbError::connection_failed(
                "Unexpected response to ValidateSession".to_string(),
            )),
        }
    }

    #[allow(clippy::result_large_err)]
    async fn login(
        &self,
        profile: &AuthProfile,
        url_callback: UrlCallback,
    ) -> Result<AuthSession, DbError> {
        let profile_json = profile_to_wire_json(profile)
            .map_err(|error| DbError::QueryFailed(error.to_string().into()))?;

        let transport = self.request_transport();

        run_request_worker(
            &self.socket_id,
            &self.provider_id,
            self.login_deadline,
            Arc::clone(&transport.abandoned),
            move || {
                let mut url_callback = Some(url_callback);

                // The provider sends its result only after the user finished in
                // the browser, so the URL is reported when its frame arrives.
                let abandoned = Arc::clone(&transport.abandoned);
                let responses = transport.send_request(
                    AuthProviderRequestBody::Login(LoginRequest { profile_json }),
                    &mut |body| {
                        if let AuthProviderResponseBody::LoginUrlProgress(progress) = body
                            && let Some(callback) = url_callback.take()
                        {
                            callback(progress.verification_url.clone());
                        }
                    },
                )?;

                if abandoned.load(Ordering::Acquire) {
                    return Err(DbError::connection_failed(
                        "Auth-provider login was abandoned".to_string(),
                    ));
                }

                login_session_from_responses(responses, url_callback)
            },
        )
        .await
    }

    async fn resolve_credentials(
        &self,
        profile: &AuthProfile,
    ) -> Result<ResolvedCredentials, DbError> {
        let profile_json = profile_to_wire_json(profile)
            .map_err(|error| DbError::QueryFailed(error.to_string().into()))?;

        let responses = self
            .send_request(AuthProviderRequestBody::ResolveCredentials(
                ResolveCredentialsRequest { profile_json },
            ))
            .await?;

        let response = responses.last().ok_or_else(|| {
            DbError::connection_failed("Auth-provider returned no response".to_string())
        })?;

        match &response.body {
            AuthProviderResponseBody::Credentials { credentials } => Ok(credentials.clone().into()),
            AuthProviderResponseBody::Error(error) => Err(error.clone().into_db_error()),
            _ => Err(DbError::connection_failed(
                "Unexpected response to ResolveCredentials".to_string(),
            )),
        }
    }

    async fn fetch_dynamic_options(
        &self,
        profile: &AuthProfile,
        request: FetchOptionsRequest,
    ) -> Result<FetchOptionsResponse, FetchOptionsError> {
        // The inherent method of the same name owns the version gate, the
        // secret stripping and the IPC round trip.
        RpcAuthProvider::fetch_dynamic_options(
            self,
            profile,
            &request.field_id,
            request.dependencies,
            request.session,
        )
        .await
        .map(|response| FetchOptionsResponse {
            options: response.options,
            cache_hint_seconds: response.cache_hint_seconds,
        })
        .map_err(fetch_options_error_from_wire)
    }
}

/// Reads the outcome of a `Login` exchange. `url_callback` is still set when
/// no `LoginUrlProgress` frame arrived; a login result then reports "no URL".
#[allow(clippy::result_large_err)]
fn login_session_from_responses(
    responses: Vec<AuthProviderResponseEnvelope>,
    mut url_callback: Option<UrlCallback>,
) -> Result<AuthSession, DbError> {
    for response in responses {
        match response.body {
            // Reported when the frame arrived.
            AuthProviderResponseBody::LoginUrlProgress(_) => {}
            AuthProviderResponseBody::LoginResult { session } => {
                if let Some(callback) = url_callback.take() {
                    callback(None);
                }

                return Ok(session.into());
            }
            AuthProviderResponseBody::Error(error) => return Err(error.into_db_error()),
            _ => {
                return Err(DbError::connection_failed(
                    "Unexpected response to Login".to_string(),
                ));
            }
        }
    }

    Err(DbError::connection_failed(
        "Auth-provider login did not return a terminal result".to_string(),
    ))
}

fn fetch_options_error_from_wire(error: FetchFieldOptionsError) -> FetchOptionsError {
    match error {
        FetchFieldOptionsError::NeedsLogin => FetchOptionsError::NeedsLogin,
        FetchFieldOptionsError::SessionExpired => FetchOptionsError::SessionExpired,
        FetchFieldOptionsError::Transient(message) => FetchOptionsError::Transient(message),
        FetchFieldOptionsError::Permanent(message) => FetchOptionsError::Permanent(message),
    }
}

#[allow(clippy::result_large_err)]
fn validate_socket_id(socket_id: &str) -> Result<(), DbError> {
    if socket_id.is_empty()
        || !socket_id
            .chars()
            .all(|char| char.is_ascii_alphanumeric() || matches!(char, '.' | '_' | '-'))
    {
        return Err(DbError::connection_failed(format!(
            "Invalid socket ID '{}': use only letters, numbers, '.', '_' or '-'",
            socket_id
        )));
    }

    auth_provider_socket_name(socket_id)
        .map(|_| ())
        .map_err(|error| DbError::connection_failed(error.to_string()))
}

#[allow(clippy::result_large_err)]
fn validate_hello_selected_version(
    selected_version: ProtocolVersion,
    supported_versions: &[ProtocolVersion],
) -> Result<(), DbError> {
    if supported_versions.contains(&selected_version) {
        return Ok(());
    }

    Err(DbError::connection_failed(format!(
        "Auth-provider selected unsupported protocol version {}.{}",
        selected_version.major, selected_version.minor
    )))
}

#[allow(clippy::result_large_err)]
fn normalize_hello_response(
    body: AuthProviderResponseBody,
) -> Result<NormalizedAuthProviderHelloResponse, DbError> {
    match body {
        AuthProviderResponseBody::Hello(hello) => Ok(NormalizedAuthProviderHelloResponse {
            provider_id: hello.provider_id,
            display_name: hello.display_name,
            form_definition: hello.form_definition,
            capabilities: AuthProviderCapabilities::default(),
            selected_version: hello.selected_version,
            secret_dependency_opt_in: false,
            audit_emit_opt_in: false,
        }),
        AuthProviderResponseBody::HelloV1_1(hello) => Ok(NormalizedAuthProviderHelloResponse {
            provider_id: hello.provider_id,
            display_name: hello.display_name,
            form_definition: hello.form_definition,
            capabilities: hello.capabilities,
            selected_version: hello.selected_version,
            secret_dependency_opt_in: false,
            audit_emit_opt_in: false,
        }),
        AuthProviderResponseBody::HelloV1_2(hello) => Ok(NormalizedAuthProviderHelloResponse {
            provider_id: hello.provider_id,
            display_name: hello.display_name,
            form_definition: hello.form_definition,
            capabilities: hello.capabilities,
            selected_version: hello.selected_version,
            secret_dependency_opt_in: hello.secret_dependency_opt_in,
            audit_emit_opt_in: false,
        }),
        AuthProviderResponseBody::HelloV1_3(hello) => Ok(NormalizedAuthProviderHelloResponse {
            provider_id: hello.provider_id,
            display_name: hello.display_name,
            form_definition: hello.form_definition,
            capabilities: hello.capabilities,
            selected_version: hello.selected_version,
            secret_dependency_opt_in: hello.secret_dependency_opt_in,
            audit_emit_opt_in: hello.audit_emit_opt_in,
        }),
        _ => Err(DbError::connection_failed(
            "Unexpected response to auth-provider Hello".to_string(),
        )),
    }
}

fn managed_hosts() -> &'static Mutex<HashMap<String, Child>> {
    static MANAGED_HOSTS: OnceLock<Mutex<HashMap<String, Child>>> = OnceLock::new();
    MANAGED_HOSTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Stops all auth-provider host processes started by DataTab. Returns count terminated.
pub fn shutdown_managed_auth_provider_hosts() -> usize {
    let mut children = {
        let Ok(mut hosts) = managed_hosts().lock() else {
            log::error!("Managed auth-provider host registry is poisoned");
            return 0;
        };
        std::mem::take(&mut *hosts)
    };

    let mut stopped = 0;

    for (socket_id, mut child) in children.drain() {
        match child.try_wait() {
            Ok(Some(status)) => {
                log::info!(
                    "Auth-provider host for '{}' already exited before shutdown ({})",
                    socket_id,
                    status
                );
            }
            Ok(None) => {
                if let Err(error) = child.kill() {
                    log::warn!(
                        "Failed to kill auth-provider host '{}': {}",
                        socket_id,
                        error
                    );
                    continue;
                }
                if let Err(error) = child.wait() {
                    log::warn!(
                        "Failed to wait for auth-provider host '{}' after kill: {}",
                        socket_id,
                        error
                    );
                }
                stopped += 1;
            }
            Err(error) => {
                log::warn!(
                    "Failed to inspect auth-provider host '{}': {}",
                    socket_id,
                    error
                );
            }
        }
    }

    stopped
}

#[allow(clippy::result_large_err)]
fn socket_is_live_for(socket_id: &str) -> Result<bool, DbError> {
    let name = auth_provider_socket_name(socket_id)
        .map_err(|error| DbError::connection_failed(error.to_string()))?;

    match IpcStream::connect(name) {
        Ok(_) => Ok(true),
        Err(_) => Ok(false),
    }
}

#[allow(clippy::result_large_err)]
fn managed_host_is_running(socket_id: &str) -> Result<bool, DbError> {
    let mut hosts = managed_hosts().lock().map_err(|_| {
        DbError::connection_failed("Managed auth-provider host registry is poisoned".to_string())
    })?;

    let mut should_remove = false;
    let is_running = if let Some(child) = hosts.get_mut(socket_id) {
        match child.try_wait().map_err(DbError::IoError)? {
            Some(_) => {
                should_remove = true;
                false
            }
            None => true,
        }
    } else {
        false
    };

    if should_remove {
        hosts.remove(socket_id);
    }

    Ok(is_running)
}

#[allow(clippy::result_large_err)]
fn register_managed_host(socket_id: &str, mut child: Child) -> Result<(), DbError> {
    let mut hosts = match managed_hosts().lock() {
        Ok(hosts) => hosts,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(DbError::connection_failed(
                "Managed auth-provider host registry is poisoned".to_string(),
            ));
        }
    };

    if let Some(mut previous) = hosts.insert(socket_id.to_string(), child)
        && let Ok(None) = previous.try_wait()
    {
        let _ = previous.kill();
        let _ = previous.wait();
    }

    Ok(())
}

#[allow(clippy::result_large_err)]
fn ensure_host_running_for(
    socket_id: &str,
    launch: Option<&IpcServiceLaunchConfig>,
) -> Result<(), DbError> {
    if socket_is_live_for(socket_id)? {
        return Ok(());
    }

    if managed_host_is_running(socket_id)? {
        let startup_timeout = launch
            .map(|config| config.startup_timeout)
            .unwrap_or_else(|| Duration::from_millis(2_000));
        let deadline = Instant::now() + startup_timeout;

        while Instant::now() < deadline {
            if socket_is_live_for(socket_id)? {
                return Ok(());
            }

            if !managed_host_is_running(socket_id)? {
                break;
            }

            thread::sleep(Duration::from_millis(75));
        }

        if managed_host_is_running(socket_id)? {
            return Err(DbError::connection_failed(format!(
                "Managed auth-provider host for '{}' is running but socket is unavailable",
                socket_id
            )));
        }
    }

    let Some(launch) = launch else {
        return Err(DbError::connection_failed(format!(
            "Auth-provider socket '{}' is not available",
            socket_id
        )));
    };

    let mut command = Command::new(&launch.program);
    command.args(&launch.args);
    command.envs(launch.env.iter().cloned());

    // The token no longer travels by environment inheritance from this
    // process; inject it explicitly when the process store has one.
    if let Some(token) = process_auth_token() {
        command.env(AUTH_PROVIDER_RPC_AUTH_TOKEN_ENV, token);
    }

    let child = command.spawn().map_err(DbError::IoError)?;
    register_managed_host(socket_id, child)?;

    let deadline = Instant::now() + launch.startup_timeout;
    while Instant::now() < deadline {
        if socket_is_live_for(socket_id)? {
            return Ok(());
        }

        if !managed_host_is_running(socket_id)? {
            return Err(DbError::connection_failed(format!(
                "Managed auth-provider host for '{}' exited before socket was ready",
                socket_id
            )));
        }

        thread::sleep(Duration::from_millis(75));
    }

    Err(DbError::connection_failed(format!(
        "Managed auth-provider host for '{}' did not become ready within {} ms",
        socket_id,
        launch.startup_timeout.as_millis()
    )))
}

pub fn negotiate_auth_provider_version(
    remote_versions: &[ProtocolVersion],
) -> Option<ProtocolVersion> {
    negotiate_highest_mutual_version(
        RpcApiFamily::AuthProviderRpc,
        auth_provider_rpc_supported_versions(),
        remote_versions,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth_provider_protocol::{
        AuthProviderHelloResponse, AuthProviderHelloResponseV1_1, AuthProviderHelloResponseV1_2,
        AuthProviderHelloResponseV1_3,
    };
    use datatab_core::auth::{
        AuthFormDef, AuthProviderCapabilities, AuthProviderLoginCapabilities,
    };
    use datatab_core::{FormFieldDef, FormFieldKind, FormSection, FormTab, RefreshTrigger};

    #[test]
    fn normalize_hello_response_defaults_v1_0_capabilities_to_disabled() {
        let normalized =
            normalize_hello_response(AuthProviderResponseBody::Hello(AuthProviderHelloResponse {
                server_name: "legacy-auth-provider".to_string(),
                server_version: "0.0.0-test".to_string(),
                selected_version: ProtocolVersion::new(1, 0),
                provider_id: "legacy-auth".to_string(),
                display_name: "Legacy Auth".to_string(),
                form_definition: AuthFormDef { tabs: vec![] },
            }))
            .expect("legacy hello should normalize");

        assert_eq!(normalized.capabilities, AuthProviderCapabilities::default());
    }

    #[test]
    fn normalize_hello_response_keeps_v1_1_capabilities() {
        let normalized = normalize_hello_response(AuthProviderResponseBody::HelloV1_1(
            AuthProviderHelloResponseV1_1 {
                server_name: "auth-provider".to_string(),
                server_version: "0.0.0-test".to_string(),
                selected_version: ProtocolVersion::new(1, 1),
                provider_id: "rpc-auth".to_string(),
                display_name: "RPC Auth".to_string(),
                form_definition: AuthFormDef { tabs: vec![] },
                capabilities: AuthProviderCapabilities {
                    login: AuthProviderLoginCapabilities {
                        supported: true,
                        verification_url_progress: true,
                    },
                },
            },
        ))
        .expect("v1.1 hello should normalize");

        assert!(normalized.capabilities.login.supported);
        assert!(normalized.capabilities.login.verification_url_progress);
    }

    #[test]
    fn normalize_hello_response_keeps_v1_2_secret_dependency_opt_in() {
        let normalized = normalize_hello_response(AuthProviderResponseBody::HelloV1_2(
            AuthProviderHelloResponseV1_2 {
                server_name: "auth-provider".to_string(),
                server_version: "0.0.0-test".to_string(),
                selected_version: ProtocolVersion::new(1, 2),
                provider_id: "rpc-auth".to_string(),
                display_name: "RPC Auth".to_string(),
                form_definition: AuthFormDef { tabs: vec![] },
                capabilities: AuthProviderCapabilities::default(),
                secret_dependency_opt_in: true,
            },
        ))
        .expect("v1.2 hello should normalize");

        assert!(normalized.secret_dependency_opt_in);
    }

    /// Builds a minimal `AuthFormDef` with a Password field and a DynamicSelect
    /// that declares a dependency on that Password field.
    fn form_with_password_dep() -> AuthFormDef {
        AuthFormDef {
            tabs: vec![FormTab {
                id: "main".to_string(),
                label: "Main".to_string(),
                sections: vec![FormSection {
                    title: "Fields".to_string(),
                    icon: None,
                    fields: vec![
                        FormFieldDef {
                            id: "api_key".to_string(),
                            label: "API Key".to_string(),
                            kind: FormFieldKind::Password,
                            placeholder: String::new(),
                            required: false,
                            default_value: String::new(),
                            enabled_when_checked: None,
                            enabled_when_unchecked: None,
                            disabled_when_field_set: None,
                            enabled_when_field_equals: None,
                            help: None,
                        },
                        FormFieldDef {
                            id: "region".to_string(),
                            label: "Region".to_string(),
                            kind: FormFieldKind::Text,
                            placeholder: String::new(),
                            required: false,
                            default_value: String::new(),
                            enabled_when_checked: None,
                            enabled_when_unchecked: None,
                            disabled_when_field_set: None,
                            enabled_when_field_equals: None,
                            help: None,
                        },
                        FormFieldDef {
                            id: "environment".to_string(),
                            label: "Environment".to_string(),
                            kind: FormFieldKind::DynamicSelect {
                                depends_on: vec!["region".to_string(), "api_key".to_string()],
                                refresh: RefreshTrigger::OnDependencyChange,
                                requires_session: false,
                                allow_freeform: false,
                            },
                            placeholder: String::new(),
                            required: false,
                            default_value: String::new(),
                            enabled_when_checked: None,
                            enabled_when_unchecked: None,
                            disabled_when_field_set: None,
                            enabled_when_field_equals: None,
                            help: None,
                        },
                    ],
                }],
            }],
        }
    }

    /// FR-SEC-01: Password fields are stripped from `dependencies` unless the
    /// provider has opted in AND the field is listed in the target's `depends_on`.
    #[test]
    fn build_fetch_dependencies_strips_password_fields_when_not_opted_in() {
        let form_def = form_with_password_dep();

        let mut raw = HashMap::new();
        raw.insert("region".to_string(), "us-east-1".to_string());
        raw.insert("api_key".to_string(), "secret-value".to_string());

        let deps = build_fetch_dependencies(&form_def, "environment", &raw, false);

        assert_eq!(
            deps.get("region").map(String::as_str),
            Some("us-east-1"),
            "non-secret dep should be included"
        );
        assert!(
            !deps.contains_key("api_key"),
            "password dep must be stripped when opt-in is false"
        );
    }

    /// FR-SEC-02: When the provider has opted in, Password fields that appear
    /// in the target field's `depends_on` are forwarded.
    #[test]
    fn build_fetch_dependencies_includes_password_when_opted_in() {
        let form_def = form_with_password_dep();

        let mut raw = HashMap::new();
        raw.insert("region".to_string(), "us-east-1".to_string());
        raw.insert("api_key".to_string(), "secret-value".to_string());

        let deps = build_fetch_dependencies(&form_def, "environment", &raw, true);

        assert_eq!(
            deps.get("api_key").map(String::as_str),
            Some("secret-value"),
            "password dep must be included when provider opted in"
        );
    }

    /// A Password field NOT listed in `depends_on` must be stripped even when
    /// `secret_dependency_opt_in` is true.
    #[test]
    fn build_fetch_dependencies_strips_password_not_in_depends_on_even_when_opted_in() {
        // Build a form that has a second password field NOT listed in environment.depends_on.
        let form_def = AuthFormDef {
            tabs: vec![FormTab {
                id: "main".to_string(),
                label: "Main".to_string(),
                sections: vec![FormSection {
                    title: "Fields".to_string(),
                    icon: None,
                    fields: vec![
                        FormFieldDef {
                            id: "api_key".to_string(),
                            label: "API Key".to_string(),
                            kind: FormFieldKind::Password,
                            placeholder: String::new(),
                            required: false,
                            default_value: String::new(),
                            enabled_when_checked: None,
                            enabled_when_unchecked: None,
                            disabled_when_field_set: None,
                            enabled_when_field_equals: None,
                            help: None,
                        },
                        // A second password field NOT referenced by environment.depends_on.
                        FormFieldDef {
                            id: "other_secret".to_string(),
                            label: "Other Secret".to_string(),
                            kind: FormFieldKind::Password,
                            placeholder: String::new(),
                            required: false,
                            default_value: String::new(),
                            enabled_when_checked: None,
                            enabled_when_unchecked: None,
                            disabled_when_field_set: None,
                            enabled_when_field_equals: None,
                            help: None,
                        },
                        FormFieldDef {
                            id: "region".to_string(),
                            label: "Region".to_string(),
                            kind: FormFieldKind::Text,
                            placeholder: String::new(),
                            required: false,
                            default_value: String::new(),
                            enabled_when_checked: None,
                            enabled_when_unchecked: None,
                            disabled_when_field_set: None,
                            enabled_when_field_equals: None,
                            help: None,
                        },
                        FormFieldDef {
                            id: "environment".to_string(),
                            label: "Environment".to_string(),
                            kind: FormFieldKind::DynamicSelect {
                                // Only api_key is in depends_on, not other_secret.
                                depends_on: vec!["region".to_string(), "api_key".to_string()],
                                refresh: RefreshTrigger::OnDependencyChange,
                                requires_session: false,
                                allow_freeform: false,
                            },
                            placeholder: String::new(),
                            required: false,
                            default_value: String::new(),
                            enabled_when_checked: None,
                            enabled_when_unchecked: None,
                            disabled_when_field_set: None,
                            enabled_when_field_equals: None,
                            help: None,
                        },
                    ],
                }],
            }],
        };

        let mut raw = HashMap::new();
        raw.insert("region".to_string(), "us-east-1".to_string());
        raw.insert("api_key".to_string(), "allowed-secret".to_string());
        raw.insert("other_secret".to_string(), "do-not-send".to_string());

        let deps = build_fetch_dependencies(&form_def, "environment", &raw, true);

        assert_eq!(
            deps.get("api_key").map(String::as_str),
            Some("allowed-secret"),
            "api_key is in depends_on so must be included when opted in"
        );
        assert!(
            !deps.contains_key("other_secret"),
            "other_secret is a Password field not in depends_on — must be stripped even when opted in"
        );
    }

    #[test]
    fn test_auth_provider_hello_v1_3_serde() {
        let response = AuthProviderHelloResponseV1_3 {
            server_name: "test-provider".to_string(),
            server_version: "1.0.0".to_string(),
            selected_version: crate::ProtocolVersion::new(1, 3),
            provider_id: "test-auth".to_string(),
            display_name: "Test Auth".to_string(),
            form_definition: AuthFormDef { tabs: vec![] },
            capabilities: AuthProviderCapabilities::default(),
            secret_dependency_opt_in: false,
            audit_emit_opt_in: true,
        };

        let json = serde_json::to_string(&response).expect("serialize v1.3 hello");
        let restored: AuthProviderHelloResponseV1_3 =
            serde_json::from_str(&json).expect("deserialize v1.3 hello");

        assert!(restored.audit_emit_opt_in);
        assert_eq!(restored.provider_id, "test-auth");
    }

    #[test]
    fn test_normalize_hello_v1_3_has_audit_emit_opt_in() {
        let normalized = normalize_hello_response(AuthProviderResponseBody::HelloV1_3(
            AuthProviderHelloResponseV1_3 {
                server_name: "test-provider".to_string(),
                server_version: "1.0.0".to_string(),
                selected_version: crate::ProtocolVersion::new(1, 3),
                provider_id: "test-auth".to_string(),
                display_name: "Test Auth".to_string(),
                form_definition: AuthFormDef { tabs: vec![] },
                capabilities: AuthProviderCapabilities::default(),
                secret_dependency_opt_in: false,
                audit_emit_opt_in: true,
            },
        ))
        .expect("v1.3 hello should normalize");

        assert!(normalized.audit_emit_opt_in);
    }

    #[test]
    fn test_normalize_hello_v1_2_has_opt_in_false() {
        let normalized = normalize_hello_response(AuthProviderResponseBody::HelloV1_2(
            AuthProviderHelloResponseV1_2 {
                server_name: "test-provider".to_string(),
                server_version: "1.0.0".to_string(),
                selected_version: crate::ProtocolVersion::new(1, 2),
                provider_id: "test-auth".to_string(),
                display_name: "Test Auth".to_string(),
                form_definition: AuthFormDef { tabs: vec![] },
                capabilities: AuthProviderCapabilities::default(),
                secret_dependency_opt_in: false,
            },
        ))
        .expect("v1.2 hello should normalize");

        assert!(
            !normalized.audit_emit_opt_in,
            "v1.2 hello must have audit_emit_opt_in == false"
        );
    }

    // =========================================================================
    // Layer C: dispatch_request_loop — audit frame interception
    // =========================================================================

    use crate::audit::{
        AuditEventEmitDto, EventCategoryDto, EventOutcomeDto, EventSeverityDto,
        ExternalAuditEmitter, ExternalAuditSource,
    };
    use std::sync::{Arc, Mutex};

    /// In-memory stream that supplies pre-encoded response bytes and absorbs writes.
    struct MockStream {
        reader: std::io::Cursor<Vec<u8>>,
        writer: Vec<u8>,
    }

    impl MockStream {
        fn new(response_bytes: Vec<u8>) -> Self {
            Self {
                reader: std::io::Cursor::new(response_bytes),
                writer: Vec::new(),
            }
        }
    }

    impl std::io::Read for MockStream {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.reader.read(buf)
        }
    }

    impl std::io::Write for MockStream {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.writer.write(buf)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.writer.flush()
        }
    }

    /// Recording emitter that captures every `emit` call for test assertions.
    #[derive(Default)]
    struct RecordingEmitter {
        calls: Arc<Mutex<Vec<ExternalAuditSource>>>,
    }

    impl RecordingEmitter {
        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    impl ExternalAuditEmitter for RecordingEmitter {
        fn emit(&self, source: ExternalAuditSource, _dto: AuditEventEmitDto) {
            self.calls.lock().unwrap().push(source);
        }
    }

    fn minimal_emit_dto() -> AuditEventEmitDto {
        AuditEventEmitDto {
            ts_ms: 1_700_000_000_000,
            level: EventSeverityDto::Info,
            category: EventCategoryDto::Connection,
            action: "connect".to_string(),
            outcome: EventOutcomeDto::Success,
            summary: "provider connected".to_string(),
            object_type: None,
            object_id: None,
            duration_ms: None,
            error_code: None,
            error_message: None,
            details_json: None,
        }
    }

    fn encode_response(envelope: &AuthProviderResponseEnvelope) -> Vec<u8> {
        let mut buf = Vec::new();
        framing::send_msg(&mut buf, envelope).expect("encode response");
        buf
    }

    fn emit_audit_frame(request_id: u64, dto: AuditEventEmitDto) -> AuthProviderResponseEnvelope {
        AuthProviderResponseEnvelope {
            protocol_version: crate::AUTH_PROVIDER_RPC_VERSION,
            request_id,
            done: false,
            body: AuthProviderResponseBody::EmitAuditEvent(dto),
        }
    }

    fn login_result_frame(request_id: u64) -> AuthProviderResponseEnvelope {
        use crate::auth_provider_protocol::AuthSessionDto;
        AuthProviderResponseEnvelope {
            protocol_version: crate::AUTH_PROVIDER_RPC_VERSION,
            request_id,
            done: true,
            body: AuthProviderResponseBody::LoginResult {
                session: AuthSessionDto {
                    provider_id: "test-auth".to_string(),
                    profile_id: uuid::Uuid::nil(),
                    expires_at: None,
                    session_data: None,
                },
            },
        }
    }

    /// Scenario P-03-a: Provider has opt-in=true and emitter attached.
    /// One EmitAuditEvent frame (done=false) followed by a terminal LoginResult (done=true).
    /// Emitter should be called once; caller receives only the LoginResult.
    #[test]
    fn test_send_request_dispatches_emit_audit_frame() {
        let emitter = Arc::new(RecordingEmitter::default());
        let provider = RpcAuthProvider::new_for_test(
            "test-sock",
            "test-auth",
            true,
            Some(emitter.clone() as Arc<dyn ExternalAuditEmitter>),
        );

        let mut response_bytes = Vec::new();
        response_bytes.extend(encode_response(&emit_audit_frame(1, minimal_emit_dto())));
        response_bytes.extend(encode_response(&login_result_frame(1)));

        let mut stream = MockStream::new(response_bytes);
        let responses = provider
            .request_transport()
            .dispatch_request_loop(
                &mut stream,
                AuthProviderRequestBody::Login(crate::auth_provider_protocol::LoginRequest {
                    profile_json: "{}".to_string(),
                }),
                &mut |_| {},
            )
            .expect("dispatch should succeed");

        assert_eq!(
            emitter.call_count(),
            1,
            "emitter must be called once for the EmitAuditEvent frame"
        );
        assert_eq!(
            responses.len(),
            1,
            "caller must receive only the terminal LoginResult"
        );
        assert!(
            matches!(
                responses[0].body,
                AuthProviderResponseBody::LoginResult { .. }
            ),
            "terminal response must be LoginResult"
        );
    }

    // =========================================================================
    // dispatch_request_loop frame cap
    // =========================================================================

    /// Builds a stream containing `count` non-done EmitAuditEvent frames with no
    /// terminal frame. Used to verify the iteration cap aborts the loop.
    fn infinite_emit_stream(count: usize) -> MockStream {
        let mut response_bytes = Vec::new();
        for _ in 0..count {
            response_bytes.extend(encode_response(&emit_audit_frame(1, minimal_emit_dto())));
        }
        MockStream::new(response_bytes)
    }

    #[test]
    fn test_dispatch_loop_terminates_on_cap() {
        let provider = RpcAuthProvider::new_for_test("test-sock", "test-auth", true, None);

        // Feed 1001 non-done frames — should abort before processing the 1001st.
        let mut stream = infinite_emit_stream(1001);

        let result = provider.request_transport().dispatch_request_loop(
            &mut stream,
            AuthProviderRequestBody::Login(crate::auth_provider_protocol::LoginRequest {
                profile_json: "{}".to_string(),
            }),
            &mut |_| {},
        );

        assert!(result.is_err(), "loop must abort after exceeding frame cap");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("frame budget") || err_msg.contains("exceeded"),
            "error must indicate frame cap reached, got: {err_msg}"
        );
    }

    #[test]
    fn test_dispatch_loop_terminates_normally() {
        let provider = RpcAuthProvider::new_for_test("test-sock", "test-auth", false, None);

        let mut response_bytes = Vec::new();
        response_bytes.extend(encode_response(&emit_audit_frame(1, minimal_emit_dto())));
        response_bytes.extend(encode_response(&emit_audit_frame(1, minimal_emit_dto())));
        response_bytes.extend(encode_response(&login_result_frame(1)));

        let mut stream = MockStream::new(response_bytes);
        let result = provider.request_transport().dispatch_request_loop(
            &mut stream,
            AuthProviderRequestBody::Login(crate::auth_provider_protocol::LoginRequest {
                profile_json: "{}".to_string(),
            }),
            &mut |_| {},
        );

        assert!(
            result.is_ok(),
            "loop must terminate normally on done=true frame"
        );
        assert_eq!(
            result.unwrap().len(),
            1,
            "must return exactly one terminal frame"
        );
    }

    // =========================================================================
    // Managed auth provider host shutdown
    // =========================================================================

    #[test]
    fn test_shutdown_kills_tracked_children() {
        use std::process::Command as StdCommand;

        // Register a sleeping child into the managed hosts registry
        let child = StdCommand::new("sleep")
            .arg("100")
            .spawn()
            .expect("spawn sleep");

        let child_id = format!("test-auth-provider-{}", child.id());
        {
            let mut hosts = managed_hosts().lock().unwrap();
            hosts.insert(child_id.clone(), child);
        }

        let stopped = shutdown_managed_auth_provider_hosts();

        assert_eq!(stopped, 1, "must report 1 stopped process");

        let hosts = managed_hosts().lock().unwrap();
        assert!(hosts.is_empty(), "registry must be empty after shutdown");
    }

    #[test]
    fn test_shutdown_returns_zero_on_empty() {
        // Ensure registry is empty first (may be populated by other tests)
        {
            let mut hosts = managed_hosts().lock().unwrap();
            hosts.clear();
        }

        let stopped = shutdown_managed_auth_provider_hosts();
        assert_eq!(stopped, 0, "must return 0 when no children tracked");
    }

    /// Scenario B-04-a: Provider has opt-in=false.
    /// EmitAuditEvent frame arrives; emitter must NOT be called.
    /// Caller receives only the terminal LoginResult.
    #[test]
    fn test_send_request_opt_in_false_drops_emit_frame() {
        let emitter = Arc::new(RecordingEmitter::default());
        let provider = RpcAuthProvider::new_for_test(
            "test-sock",
            "test-auth",
            false,
            Some(emitter.clone() as Arc<dyn ExternalAuditEmitter>),
        );

        let mut response_bytes = Vec::new();
        response_bytes.extend(encode_response(&emit_audit_frame(1, minimal_emit_dto())));
        response_bytes.extend(encode_response(&login_result_frame(1)));

        let mut stream = MockStream::new(response_bytes);
        let responses = provider
            .request_transport()
            .dispatch_request_loop(
                &mut stream,
                AuthProviderRequestBody::Login(crate::auth_provider_protocol::LoginRequest {
                    profile_json: "{}".to_string(),
                }),
                &mut |_| {},
            )
            .expect("dispatch should succeed even with opt-in=false");

        assert_eq!(
            emitter.call_count(),
            0,
            "emitter must NOT be called when audit_emit_opt_in=false"
        );
        assert_eq!(
            responses.len(),
            1,
            "caller must still receive the terminal LoginResult"
        );
        assert!(
            matches!(
                responses[0].body,
                AuthProviderResponseBody::LoginResult { .. }
            ),
            "terminal response must be LoginResult"
        );
    }

    // =========================================================================
    // Dynamic options through `dyn DynAuthProvider` (#347)
    // =========================================================================

    use crate::auth_provider_protocol::{AuthSessionDto, FetchFieldOptionsResponse};
    use datatab_core::SelectOption;

    fn fake_options_response() -> FetchFieldOptionsResponse {
        FetchFieldOptionsResponse {
            options: vec![
                SelectOption::new("dev", "Development"),
                SelectOption::new("prod", "Production"),
            ],
            cache_hint_seconds: Some(60),
        }
    }

    /// A provider on a real local socket. It decodes with the protocol types,
    /// declares its form in its v1.2 `Hello`, and records each request, so a
    /// test can check what crossed the IPC boundary.
    struct FakeProviderServer {
        socket_id: String,
        requests: Arc<Mutex<Vec<AuthProviderRequestBody>>>,
        /// Set when the server holds each `Login` answer until told to send it.
        login_release: Option<std::sync::mpsc::Sender<()>>,
    }

    /// The request kind a silent server never answers.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum SilentOn {
        Nothing,
        Hello,
        Login,
        FetchOptions,
        /// Answers `Login` after `release_login`, sending the URL frame only then.
        LoginUrlAfterRelease,
    }

    /// The URL a holding server reports before it waits.
    const HELD_LOGIN_VERIFICATION_URL: &str = "https://verify.example/device";

    /// How long a holding server waits for `release_login`. A client that
    /// blocks its caller until the answer never gets to release it; the
    /// fallback lets that run fail instead of hanging.
    const HELD_LOGIN_FALLBACK: Duration = Duration::from_secs(6);

    /// How long a test waits for the URL of a held login. Shorter than the
    /// fallback, so a URL that arrives in time was sent before the answer.
    const HELD_LOGIN_URL_WAIT: Duration = Duration::from_secs(2);

    impl FakeProviderServer {
        /// Starts a provider that declares `form_with_password_dep()`.
        fn start(secret_dependency_opt_in: bool) -> Self {
            Self::start_with_form(form_with_password_dep(), secret_dependency_opt_in)
        }

        fn start_with_form(form_definition: AuthFormDef, secret_dependency_opt_in: bool) -> Self {
            Self::start_server(
                form_definition,
                secret_dependency_opt_in,
                false,
                SilentOn::Nothing,
            )
        }

        /// Starts a provider that answers `Hello` but holds the socket open and
        /// never answers the requests `silent_on` names.
        fn start_silent(silent_on: SilentOn) -> Self {
            Self::start_server(form_with_password_dep(), false, false, silent_on)
        }

        /// Starts a provider that sends the URL frame and the result of a `Login`
        /// only after `release_login`.
        fn start_late_login() -> Self {
            Self::start_server(
                form_with_password_dep(),
                false,
                true,
                SilentOn::LoginUrlAfterRelease,
            )
        }

        /// Starts a provider that answers `Login` as one waiting for a browser
        /// step does: it reports the verification URL, then sends the result
        /// only after `release_login`.
        fn start_holding_login() -> Self {
            Self::start_server(form_with_password_dep(), false, true, SilentOn::Nothing)
        }

        fn start_server(
            form_definition: AuthFormDef,
            secret_dependency_opt_in: bool,
            hold_login: bool,
            silent_on: SilentOn,
        ) -> Self {
            use interprocess::local_socket::traits::Listener;
            use interprocess::local_socket::{ListenerNonblockingMode, ListenerOptions};

            let socket_id = format!("options-test-{}", uuid::Uuid::new_v4().simple());
            let name = auth_provider_socket_name(&socket_id).expect("socket name");
            let listener = ListenerOptions::new()
                .name(name)
                .nonblocking(ListenerNonblockingMode::Neither)
                .create_sync()
                .expect("listener");

            let requests = Arc::new(Mutex::new(Vec::new()));
            let recorded = Arc::clone(&requests);

            let (login_release, login_released) = std::sync::mpsc::channel::<()>();
            let login_release = hold_login.then_some(login_release);
            let login_released = hold_login.then_some(login_released);

            thread::spawn(move || {
                // Streams of silent requests stay open, so the client sees no close.
                let mut kept_open = Vec::new();
                loop {
                    let Ok(mut stream) = listener.accept() else {
                        continue;
                    };

                    // Liveness probes connect and close without sending a
                    // request, and a request that does not decode ends here.
                    let Ok(request) =
                        framing::recv_msg::<_, AuthProviderRequestEnvelope>(&mut stream)
                    else {
                        continue;
                    };

                    let silent = match request.body {
                        AuthProviderRequestBody::Hello(_) => silent_on == SilentOn::Hello,
                        AuthProviderRequestBody::Login(_) => silent_on == SilentOn::Login,
                        AuthProviderRequestBody::FetchDynamicOptions(_) => {
                            silent_on == SilentOn::FetchOptions
                        }
                        _ => false,
                    };
                    if silent {
                        kept_open.push(stream);
                        continue;
                    }

                    let Some(body) = fake_provider_answer(
                        &request.body,
                        &form_definition,
                        secret_dependency_opt_in,
                    ) else {
                        continue;
                    };
                    let held_login = login_released
                        .as_ref()
                        .filter(|_| matches!(request.body, AuthProviderRequestBody::Login(_)));
                    recorded.lock().expect("lock").push(request.body);

                    let late_url = silent_on == SilentOn::LoginUrlAfterRelease;
                    if let Some(login_released) = held_login {
                        if late_url && login_released.recv_timeout(HELD_LOGIN_FALLBACK).is_err() {
                            eprintln!("fake provider answers a login nobody released");
                        }
                        let progress = AuthProviderResponseEnvelope::login_url_progress(
                            request.protocol_version,
                            request.request_id,
                            Some(HELD_LOGIN_VERIFICATION_URL.to_string()),
                        );
                        if let Err(error) = framing::send_msg(&mut stream, &progress) {
                            eprintln!("fake provider could not report the URL: {error}");
                        }
                        if !late_url && login_released.recv_timeout(HELD_LOGIN_FALLBACK).is_err() {
                            eprintln!("fake provider answers a login nobody released");
                        }
                    }

                    let response = AuthProviderResponseEnvelope::ok(
                        request.protocol_version,
                        request.request_id,
                        body,
                    );
                    if let Err(error) = framing::send_msg(&mut stream, &response) {
                        eprintln!("fake provider could not answer: {error}");
                    }
                }
            });

            Self {
                socket_id,
                requests,
                login_release,
            }
        }

        /// Lets a holding server send the `Login` result it is waiting with.
        fn release_login(&self) {
            self.login_release
                .as_ref()
                .expect("the server holds its login answers")
                .send(())
                .expect("the server is waiting to answer");
        }

        /// Probes the server and returns the provider the way the app holds
        /// it: behind the trait object.
        fn probe(&self) -> Arc<dyn DynAuthProvider> {
            Arc::new(RpcAuthProvider::probe(&self.socket_id, None).expect("probe"))
        }

        fn fetch_requests(&self) -> Vec<FetchFieldOptionsRequest> {
            self.requests
                .lock()
                .expect("lock")
                .iter()
                .filter_map(|body| match body {
                    AuthProviderRequestBody::FetchDynamicOptions(fetch) => Some(fetch.clone()),
                    _ => None,
                })
                .collect()
        }

        fn login_requests(&self) -> Vec<LoginRequest> {
            self.requests
                .lock()
                .expect("lock")
                .iter()
                .filter_map(|body| match body {
                    AuthProviderRequestBody::Login(login) => Some(login.clone()),
                    _ => None,
                })
                .collect()
        }
    }

    fn fake_provider_answer(
        body: &AuthProviderRequestBody,
        form_definition: &AuthFormDef,
        secret_dependency_opt_in: bool,
    ) -> Option<AuthProviderResponseBody> {
        match body {
            AuthProviderRequestBody::Hello(_) => Some(AuthProviderResponseBody::HelloV1_2(
                AuthProviderHelloResponseV1_2 {
                    server_name: "fake-provider".to_string(),
                    server_version: "0.0.0-test".to_string(),
                    selected_version: AUTH_PROVIDER_RPC_V1_2,
                    provider_id: "test-auth".to_string(),
                    display_name: "Test Provider".to_string(),
                    form_definition: form_definition.clone(),
                    capabilities: AuthProviderCapabilities::default(),
                    secret_dependency_opt_in,
                },
            )),
            AuthProviderRequestBody::FetchDynamicOptions(_) => Some(
                AuthProviderResponseBody::DynamicOptions(fake_options_response()),
            ),
            AuthProviderRequestBody::Login(_) => Some(AuthProviderResponseBody::LoginResult {
                session: AuthSessionDto {
                    provider_id: "test-auth".to_string(),
                    profile_id: uuid::Uuid::nil(),
                    expires_at: None,
                    session_data: None,
                },
            }),
            _ => None,
        }
    }

    /// The request the Settings form builds: the null session marker, and a
    /// dependency map that still holds the secret input.
    fn environment_options_request() -> FetchOptionsRequest {
        FetchOptionsRequest {
            field_id: "environment".to_string(),
            dependencies: HashMap::from([
                ("region".to_string(), "us-east-1".to_string()),
                ("api_key".to_string(), "secret-value".to_string()),
            ]),
            session: Some(serde_json::Value::Null),
        }
    }

    fn options_profile() -> AuthProfile {
        AuthProfile::new("rpc profile", "test-auth", HashMap::new())
    }

    /// The Settings form's call over a real socket: the provider declares the
    /// field in its manifest, the fetch reaches it through the trait object,
    /// and a provider that did not opt in never receives a secret dependency.
    #[tokio::test]
    async fn trait_fetch_dynamic_options_reaches_provider_as_the_settings_form_calls_it() {
        let server = FakeProviderServer::start(false);
        let provider = server.probe();

        let declares_dynamic_select = provider
            .form_def()
            .tabs
            .iter()
            .flat_map(|tab| tab.sections.iter())
            .flat_map(|section| section.fields.iter())
            .any(|field| {
                field.id == "environment"
                    && matches!(field.kind, FormFieldKind::DynamicSelect { .. })
            });
        assert!(
            declares_dynamic_select,
            "the manifest's DynamicSelect field must reach form_def()"
        );

        let response = provider
            .fetch_dynamic_options(&options_profile(), environment_options_request())
            .await
            .expect("options from the RPC provider");

        assert_eq!(response.options, fake_options_response().options);
        assert_eq!(response.cache_hint_seconds, Some(60));

        let requests = server.fetch_requests();
        let [request] = requests.as_slice() else {
            panic!("expected one recorded request, got {}", requests.len());
        };
        assert_eq!(request.field_id, "environment");
        assert_eq!(
            request.dependencies.get("region").map(String::as_str),
            Some("us-east-1"),
            "non-secret dependency must reach the provider"
        );
        assert!(
            !request.dependencies.contains_key("api_key"),
            "secret dependency must be stripped when the provider did not opt in"
        );
        assert_eq!(
            request.session, None,
            "the form's null marker is sent as no session"
        );
    }

    /// Session data cannot cross the postcard wire, so the host refuses it
    /// before it sends anything.
    #[tokio::test]
    async fn trait_fetch_dynamic_options_rejects_session_data_before_sending() {
        let server = FakeProviderServer::start(false);
        let provider = server.probe();
        let request = FetchOptionsRequest {
            session: Some(serde_json::json!({ "account": "dev" })),
            ..environment_options_request()
        };

        let error = provider
            .fetch_dynamic_options(&options_profile(), request)
            .await
            .expect_err("session data is not supported");

        let FetchOptionsError::Permanent(message) = &error else {
            panic!("expected a permanent error, got {error:?}");
        };
        assert!(
            message.contains("session data"),
            "expected the session message, got: {message}"
        );
        assert!(
            server.fetch_requests().is_empty(),
            "nothing may be sent to the provider"
        );
    }

    /// A provider that opted in receives the secret dependency its field lists.
    #[tokio::test]
    async fn trait_fetch_dynamic_options_forwards_secret_dependency_when_opted_in() {
        let server = FakeProviderServer::start(true);
        let provider = server.probe();

        provider
            .fetch_dynamic_options(&options_profile(), environment_options_request())
            .await
            .expect("options from the RPC provider");

        let requests = server.fetch_requests();
        let [request] = requests.as_slice() else {
            panic!("expected one recorded request, got {}", requests.len());
        };
        assert_eq!(
            request.dependencies.get("api_key").map(String::as_str),
            Some("secret-value"),
            "secret dependency must reach a provider that opted in"
        );
    }

    /// Below v1.2 the host answers "not supported" itself. The message is the
    /// version gate's, which tells it apart from the trait default.
    #[tokio::test]
    async fn trait_fetch_dynamic_options_is_not_supported_below_v1_2() {
        let mut provider =
            RpcAuthProvider::new_for_test("options-test-no-listener", "test-auth", false, None);
        provider.selected_version = crate::envelope::AUTH_PROVIDER_RPC_V1_1;
        let provider: Arc<dyn DynAuthProvider> = Arc::new(provider);

        let error = provider
            .fetch_dynamic_options(&options_profile(), environment_options_request())
            .await
            .expect_err("a v1.1 provider has no dynamic options");

        let FetchOptionsError::Permanent(message) = &error else {
            panic!("expected a permanent error, got {error:?}");
        };
        assert!(
            message.contains("does not support FetchDynamicOptions"),
            "expected the version gate message, got: {message}"
        );
    }

    /// The two places a profile can hold the secret: inline in `fields`, as
    /// the Settings form snapshot does, and in `secret_fields`, as a stored
    /// profile does.
    fn profiles_with_secret() -> [AuthProfile; 2] {
        let region = ("region".to_string(), "us-east-1".to_string());

        let form_snapshot = AuthProfile::new(
            "form snapshot",
            "test-auth",
            HashMap::from([
                region.clone(),
                ("api_key".to_string(), "secret-value".to_string()),
            ]),
        );

        let mut stored = AuthProfile::new("stored", "test-auth", HashMap::from([region]));
        stored.secret_fields.insert(
            "api_key".to_string(),
            secrecy::SecretString::from("secret-value".to_string()),
        );

        [form_snapshot, stored]
    }

    /// The `fields` object of a profile as the provider received it.
    fn wire_profile_fields(profile_json: &str) -> serde_json::Map<String, serde_json::Value> {
        let profile: serde_json::Value =
            serde_json::from_str(profile_json).expect("profile_json is JSON");

        profile
            .get("fields")
            .and_then(serde_json::Value::as_object)
            .cloned()
            .expect("profile_json has a fields object")
    }

    /// A provider that did not opt in gets the profile without secret values.
    #[tokio::test]
    async fn trait_fetch_dynamic_options_withholds_profile_secrets_without_opt_in() {
        let server = FakeProviderServer::start(false);
        let provider = server.probe();

        for profile in profiles_with_secret() {
            provider
                .fetch_dynamic_options(&profile, environment_options_request())
                .await
                .expect("options from the RPC provider");
        }

        let requests = server.fetch_requests();
        assert_eq!(requests.len(), 2);
        for request in requests {
            let fields = wire_profile_fields(&request.profile_json);
            assert_eq!(
                fields.get("region").and_then(serde_json::Value::as_str),
                Some("us-east-1"),
                "non-secret profile field must reach the provider"
            );
            assert!(
                !fields.contains_key("api_key"),
                "secret profile field must be withheld when the provider did not opt in"
            );
            assert!(
                !request.profile_json.contains("secret-value"),
                "the secret value must not appear anywhere in profile_json"
            );
        }
    }

    /// A provider that opted in gets the secret its field depends on.
    #[tokio::test]
    async fn trait_fetch_dynamic_options_sends_profile_secret_when_opted_in() {
        let server = FakeProviderServer::start(true);
        let provider = server.probe();

        for profile in profiles_with_secret() {
            provider
                .fetch_dynamic_options(&profile, environment_options_request())
                .await
                .expect("options from the RPC provider");
        }

        let requests = server.fetch_requests();
        assert_eq!(requests.len(), 2);
        for request in requests {
            let fields = wire_profile_fields(&request.profile_json);
            assert_eq!(
                fields.get("api_key").and_then(serde_json::Value::as_str),
                Some("secret-value"),
                "a provider that opted in must receive the secret its field depends on"
            );
        }
    }

    /// `form_with_password_dep()` plus a secret the `DynamicSelect` does not
    /// depend on and a reference to another profile.
    fn form_with_unlisted_secret_and_profile_ref() -> AuthFormDef {
        let mut form = form_with_password_dep();
        let section = form
            .tabs
            .iter_mut()
            .flat_map(|tab| tab.sections.iter_mut())
            .next()
            .expect("form has a section");
        let template = section.fields.first().cloned().expect("form has a field");

        section.fields.push(FormFieldDef {
            id: "other_secret".to_string(),
            label: "Other Secret".to_string(),
            kind: FormFieldKind::Password,
            ..template.clone()
        });
        section.fields.push(FormFieldDef {
            id: "shared_ref".to_string(),
            label: "Shared Profile".to_string(),
            kind: FormFieldKind::AuthProfileRef { provider_id: None },
            ..template
        });

        form
    }

    /// The opt-in releases only the secrets the target field depends on.
    #[tokio::test]
    async fn trait_fetch_dynamic_options_withholds_secret_outside_depends_on_when_opted_in() {
        let server =
            FakeProviderServer::start_with_form(form_with_unlisted_secret_and_profile_ref(), true);
        let provider = server.probe();

        let [mut form_snapshot, mut stored] = profiles_with_secret();
        form_snapshot
            .fields
            .insert("other_secret".to_string(), "other-secret-value".to_string());
        stored.secret_fields.insert(
            "other_secret".to_string(),
            secrecy::SecretString::from("other-secret-value".to_string()),
        );

        let mut request = environment_options_request();
        request
            .dependencies
            .insert("other_secret".to_string(), "other-secret-value".to_string());

        for profile in [form_snapshot, stored] {
            provider
                .fetch_dynamic_options(&profile, request.clone())
                .await
                .expect("options from the RPC provider");
        }

        let requests = server.fetch_requests();
        assert_eq!(requests.len(), 2);
        for request in requests {
            let fields = wire_profile_fields(&request.profile_json);
            assert_eq!(
                fields.get("api_key").and_then(serde_json::Value::as_str),
                Some("secret-value"),
                "the secret the field depends on is released"
            );
            assert!(
                !fields.contains_key("other_secret"),
                "a secret outside depends_on must stay out of the profile"
            );
            assert!(
                !request.dependencies.contains_key("other_secret"),
                "a secret outside depends_on must stay out of the dependency map"
            );
            assert!(
                !request.profile_json.contains("other-secret-value"),
                "the unlisted secret value must not appear anywhere in profile_json"
            );
        }
    }

    /// A referenced profile can still hold a secret as plaintext in `fields`
    /// while its keyring migration is deferred. `expand_auth_profile_refs`
    /// copies it into the snapshot under a key this provider's form does not
    /// declare, so it must never be sent, with or without the opt-in.
    #[tokio::test]
    async fn trait_fetch_dynamic_options_never_sends_fields_the_form_does_not_declare() {
        let form = form_with_unlisted_secret_and_profile_ref();

        let referenced = AuthProfile::new(
            "shared",
            "other-auth",
            HashMap::from([(
                "legacy_token".to_string(),
                "legacy-secret-value".to_string(),
            )]),
        );
        let consumer = AuthProfile::new(
            "form snapshot",
            "test-auth",
            HashMap::from([
                ("region".to_string(), "us-east-1".to_string()),
                ("shared_ref".to_string(), referenced.id.to_string()),
            ]),
        );
        let snapshot = datatab_core::auth::expand_auth_profile_refs(&consumer, &form, &|id| {
            (*id == referenced.id).then(|| referenced.clone())
        });
        assert_eq!(
            snapshot.fields.get("legacy_token").map(String::as_str),
            Some("legacy-secret-value"),
            "the expansion copies the referenced profile's field into the snapshot"
        );

        for secret_dependency_opt_in in [false, true] {
            let server =
                FakeProviderServer::start_with_form(form.clone(), secret_dependency_opt_in);
            let provider = server.probe();

            provider
                .fetch_dynamic_options(&snapshot, environment_options_request())
                .await
                .expect("options from the RPC provider");

            let requests = server.fetch_requests();
            let [request] = requests.as_slice() else {
                panic!("expected one recorded request, got {}", requests.len());
            };
            let fields = wire_profile_fields(&request.profile_json);
            assert!(
                !request.profile_json.contains("legacy-secret-value"),
                "a referenced legacy secret must never reach the provider \
                 (opt-in: {secret_dependency_opt_in})"
            );
            assert!(
                !fields.contains_key("legacy_token"),
                "a field the form does not declare must be dropped"
            );
            assert_eq!(
                fields.get("region").and_then(serde_json::Value::as_str),
                Some("us-east-1"),
                "a declared non-secret field must reach the provider"
            );
            assert_eq!(
                fields.get("shared_ref").and_then(serde_json::Value::as_str),
                Some(referenced.id.to_string().as_str()),
                "the declared reference field must reach the provider"
            );
        }
    }

    /// Login needs the secret to authenticate: it keeps the full profile,
    /// whatever the opt-in.
    #[tokio::test]
    async fn trait_login_still_sends_the_full_profile() {
        let server = FakeProviderServer::start(false);
        let provider = server.probe();

        for profile in profiles_with_secret() {
            provider
                .login(&profile, Box::new(|_| {}))
                .await
                .expect("login result from the RPC provider");
        }

        let requests = server.login_requests();
        assert_eq!(requests.len(), 2);
        for request in requests {
            let fields = wire_profile_fields(&request.profile_json);
            assert_eq!(
                fields.get("api_key").and_then(serde_json::Value::as_str),
                Some("secret-value"),
                "login must still carry the secret"
            );
        }
    }

    // =========================================================================
    // Requests run off the calling thread (#369)
    // =========================================================================

    /// Polls `future` once, as an executor thread does. No tokio runtime is
    /// involved: the GPUI executors have none.
    fn poll_once<Output>(
        future: &mut std::pin::Pin<Box<dyn Future<Output = Output> + Send + '_>>,
    ) -> std::task::Poll<Output> {
        let mut context = std::task::Context::from_waker(futures::task::noop_waker_ref());
        future.as_mut().poll(&mut context)
    }

    /// The Settings form and the connection manager await `login` on the UI
    /// thread. While the provider waits for the browser step, the future must
    /// hand the thread back instead of blocking it on the socket.
    #[test]
    fn login_hands_the_calling_thread_back_while_the_provider_waits() {
        let server = FakeProviderServer::start_holding_login();
        let provider = server.probe();
        let profile = options_profile();

        let mut login = provider.login(&profile, Box::new(|_| {}));

        assert!(
            poll_once(&mut login).is_pending(),
            "login blocked the calling thread until the provider answered"
        );

        server.release_login();
        let session = futures::executor::block_on(login).expect("login result");
        assert_eq!(session.provider_id, "test-auth");
    }

    const SHORT_TEST_DEADLINE: Duration = Duration::from_millis(150);
    const LONG_TEST_DEADLINE: Duration = Duration::from_secs(5);

    fn provider_with_deadlines(
        server: &FakeProviderServer,
        request_deadline: Duration,
        login_deadline: Duration,
    ) -> Arc<dyn DynAuthProvider> {
        Arc::new(
            RpcAuthProvider::probe(&server.socket_id, None)
                .expect("probe")
                .with_deadlines(request_deadline, login_deadline),
        )
    }

    /// A login waits for the browser step for longer than any other request
    /// may take, and still succeeds.
    #[test]
    fn login_may_outlive_the_request_deadline() {
        let server = FakeProviderServer::start_holding_login();
        let provider = provider_with_deadlines(&server, SHORT_TEST_DEADLINE, LONG_TEST_DEADLINE);
        let profile = options_profile();

        // The wait starts when the provider has the request, not before the call.
        let release = server.login_release.clone().expect("holding server");
        let received = Arc::clone(&server.requests);
        thread::spawn(move || {
            while !received
                .lock()
                .expect("lock")
                .iter()
                .any(|body| matches!(body, AuthProviderRequestBody::Login(_)))
            {
                thread::sleep(Duration::from_millis(5));
            }
            thread::sleep(SHORT_TEST_DEADLINE * 3);
            if release.send(()).is_err() {
                eprintln!("the fake provider stopped waiting");
            }
        });

        let session = futures::executor::block_on(provider.login(&profile, Box::new(|_| {})))
            .expect("a login that outlives the request deadline succeeds");
        assert_eq!(session.provider_id, "test-auth");
    }

    /// After the caller got the deadline error, a late URL frame and a late
    /// result must not reach its callback.
    #[test]
    fn a_login_answered_after_the_deadline_runs_no_callback() {
        let server = FakeProviderServer::start_late_login();
        let provider = provider_with_deadlines(&server, LONG_TEST_DEADLINE, SHORT_TEST_DEADLINE);

        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let result = futures::executor::block_on(provider.login(
            &options_profile(),
            Box::new(move |_| {
                counted.fetch_add(1, Ordering::SeqCst);
            }),
        ));
        assert!(result.is_err(), "the deadline must end the login");

        server.release_login();
        // The worker handles both frames within milliseconds of the release.
        thread::sleep(Duration::from_millis(500));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "the callback ran after the caller gave up"
        );
    }

    /// A request that finished must not leave its timer asleep until the deadline.
    #[test]
    fn the_deadline_timer_ends_when_the_worker_finishes() {
        let (worker_done, timer_done) = std::sync::mpsc::channel::<()>();
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<Option<Result<(), DbError>>>(2);
        let (ended, timer_ended) = std::sync::mpsc::channel::<()>();

        thread::spawn(move || {
            run_deadline_timer(&timer_done, Duration::from_secs(60), &sender);
            if ended.send(()).is_err() {
                eprintln!("the test stopped waiting for the timer");
            }
        });

        drop(worker_done);

        timer_ended
            .recv_timeout(Duration::from_secs(5))
            .expect("the timer must end when the worker finishes");
        assert!(
            receiver.try_recv().is_err(),
            "a finished request must not send a timeout"
        );
    }

    #[test]
    fn a_request_the_provider_never_answers_fails_at_the_request_deadline() {
        let server = FakeProviderServer::start_silent(SilentOn::FetchOptions);
        let provider = provider_with_deadlines(&server, SHORT_TEST_DEADLINE, LONG_TEST_DEADLINE);

        let started = Instant::now();
        let error = futures::executor::block_on(
            provider.fetch_dynamic_options(&options_profile(), environment_options_request()),
        )
        .expect_err("a silent provider must hit the deadline");

        assert!(started.elapsed() < LONG_TEST_DEADLINE);
        assert!(
            format!("{error:?}").contains("did not answer within"),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn probe_worker_disconnect_is_not_reported_as_timeout() {
        let (sender, receiver) = std::sync::mpsc::channel();
        drop(sender);
        let error = receive_probe_result(receiver, Duration::from_secs(1), "test")
            .expect_err("worker ended without a result");
        assert!(error.to_string().contains("worker ended without a result"));
    }

    #[test]
    fn probe_deadline_includes_managed_service_startup_timeout() {
        let launch = IpcServiceLaunchConfig {
            program: "provider".into(),
            args: vec![],
            env: vec![],
            startup_timeout: Duration::from_secs(7),
        };
        assert_eq!(probe_deadline(None), PROBE_DEADLINE);
        assert_eq!(
            probe_deadline(Some(&launch)),
            PROBE_DEADLINE + Duration::from_secs(7)
        );
    }

    #[test]
    fn multiple_probes_finish_together_and_keep_input_order() {
        let first = FakeProviderServer::start_silent(SilentOn::Hello);
        let responsive = FakeProviderServer::start_silent(SilentOn::Nothing);
        let last = FakeProviderServer::start_silent(SilentOn::Hello);
        let sockets = vec![first.socket_id, responsive.socket_id, last.socket_id];
        let (sender, receiver) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let services: Vec<_> = sockets.into_iter().map(|socket| (socket, None)).collect();
            let results = RpcAuthProvider::probe_many_with(&services, |socket, _| {
                RpcAuthProvider::probe_within(socket, None, Duration::from_secs(1))
                    .map_err(Box::new)
            });
            sender.send(results).expect("send results");
        });
        let results = receiver
            .recv_timeout(Duration::from_millis(1_800))
            .expect("all probes must finish within one deadline");
        assert!(results[0].is_err());
        assert_eq!(
            results[1]
                .as_ref()
                .expect("responsive provider")
                .provider_id,
            "test-auth"
        );
        assert!(results[2].is_err());
    }

    #[test]
    fn a_probe_the_provider_never_answers_fails_at_the_probe_deadline() {
        let server = FakeProviderServer::start_silent(SilentOn::Hello);

        let started = Instant::now();
        let error = RpcAuthProvider::probe_within(&server.socket_id, None, SHORT_TEST_DEADLINE)
            .err()
            .expect("a provider that never answers Hello must hit the probe deadline");

        assert!(started.elapsed() < LONG_TEST_DEADLINE);
        assert!(
            format!("{error:?}").contains("did not answer Hello within"),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn a_probe_the_provider_answers_registers_within_the_deadline() {
        let server = FakeProviderServer::start_silent(SilentOn::Nothing);

        let provider = RpcAuthProvider::probe_within(&server.socket_id, None, LONG_TEST_DEADLINE)
            .expect("a provider that answers Hello probes");
        assert_eq!(provider.provider_id, "test-auth");
    }

    #[test]
    fn a_login_the_provider_never_answers_fails_at_the_login_deadline() {
        let server = FakeProviderServer::start_silent(SilentOn::Login);
        let provider = provider_with_deadlines(&server, LONG_TEST_DEADLINE, SHORT_TEST_DEADLINE);

        let started = Instant::now();
        let error =
            futures::executor::block_on(provider.login(&options_profile(), Box::new(|_| {})))
                .expect_err("a silent provider must hit the login deadline");

        assert!(started.elapsed() < LONG_TEST_DEADLINE);
        assert!(
            format!("{error:?}").contains("did not answer within"),
            "unexpected error: {error:?}"
        );
    }

    /// The verification URL must reach the caller while the provider still
    /// waits: the Settings form shows Cancel only once it has the URL.
    #[test]
    fn login_reports_the_verification_url_while_the_provider_waits() {
        let server = FakeProviderServer::start_holding_login();
        let provider = server.probe();
        let profile = options_profile();

        let (url_sender, url_receiver) = std::sync::mpsc::channel();
        let mut login = provider.login(
            &profile,
            Box::new(move |url| {
                if url_sender.send(url).is_err() {
                    eprintln!("the test stopped waiting for the URL");
                }
            }),
        );

        assert!(
            poll_once(&mut login).is_pending(),
            "login blocked the calling thread until the provider answered"
        );

        // The server answers only after `release_login` below, so a URL that
        // arrives here arrived before the login result.
        let url = url_receiver
            .recv_timeout(HELD_LOGIN_URL_WAIT)
            .expect("the URL must arrive before the provider answers");
        assert_eq!(url.as_deref(), Some(HELD_LOGIN_VERIFICATION_URL));

        server.release_login();
        futures::executor::block_on(login).expect("login result");
        assert!(
            url_receiver.try_recv().is_err(),
            "the URL callback must run once"
        );
    }

    /// The Settings form shows a different hint for each kind, so none may
    /// change on the way from the wire type to the core type.
    #[test]
    fn fetch_options_error_from_wire_keeps_each_kind() {
        assert_eq!(
            fetch_options_error_from_wire(FetchFieldOptionsError::NeedsLogin),
            FetchOptionsError::NeedsLogin
        );
        assert_eq!(
            fetch_options_error_from_wire(FetchFieldOptionsError::SessionExpired),
            FetchOptionsError::SessionExpired
        );
        assert_eq!(
            fetch_options_error_from_wire(FetchFieldOptionsError::Transient("busy".to_string())),
            FetchOptionsError::Transient("busy".to_string())
        );
        assert_eq!(
            fetch_options_error_from_wire(FetchFieldOptionsError::Permanent("no".to_string())),
            FetchOptionsError::Permanent("no".to_string())
        );
    }
}
