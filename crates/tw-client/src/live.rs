use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tw_core::{Error, Store, secret};
use tw_hv::{Endpoint, HostKey, Hypervisor, VmHandle, VmState};
use tw_transport::{Target, Transport};
use uuid::Uuid;

use crate::{ClientError, FrameStream, Guest, GuestClient, GuestError, Reply};

type TargetFor = Box<dyn Fn(Uuid) -> Target + Send + Sync>;

/// A guest reached through the hypervisor and a transport. It checks the VM is running, looks up its id and
/// secret, connects once, and reconnects when the connection has gone away.
pub struct LiveGuest {
    store: Store,
    name: String,
    hypervisor: Arc<dyn Hypervisor>,
    transport: Arc<dyn Transport>,
    target: TargetFor,
    connection: Mutex<Option<Arc<GuestClient>>>,
}

impl LiveGuest {
    pub fn new(
        store: Store,
        name: impl Into<String>,
        hypervisor: Arc<dyn Hypervisor>,
        transport: Arc<dyn Transport>,
        service_id: Uuid,
    ) -> Self {
        Self {
            store,
            name: name.into(),
            hypervisor,
            transport,
            target: Box::new(move |vm_id| Target::HvSock { vm_id, service_id }),
            connection: Mutex::new(None),
        }
    }

    /// Replaces how a VM id becomes a connection target, for tests that use a loopback socket.
    pub fn with_target(mut self, target: impl Fn(Uuid) -> Target + Send + Sync + 'static) -> Self {
        self.target = Box::new(target);
        self
    }

    fn handle(&self) -> Result<VmHandle, GuestError> {
        match self.store.load(&self.name) {
            Ok(machine) => Ok(VmHandle::new(machine.hv_name)),
            Err(Error::NoSuchVm(name)) => Err(GuestError::NotCreated(name)),
            Err(other) => Err(GuestError::Other(other.to_string())),
        }
    }

    async fn require_running(&self, vm: &VmHandle) -> Result<(), GuestError> {
        match self.hypervisor.state(vm).await.map_err(|e| GuestError::Other(e.to_string()))? {
            VmState::Running => Ok(()),
            other => Err(GuestError::NotRunning { name: self.name.clone(), state: other.as_str().to_string() }),
        }
    }

    /// Where to connect and the shared secret, after checking the VM is running.
    async fn endpoint(&self) -> Result<(Target, Vec<u8>), GuestError> {
        let vm = self.handle()?;
        self.require_running(&vm).await?;
        let endpoint = self.hypervisor.endpoint(&vm).await.map_err(|e| GuestError::Other(e.to_string()))?;
        let directory = self.store.directory(&self.name).map_err(|e| GuestError::Other(e.to_string()))?;
        let key = secret::load(&directory.join("channel.secret")).map_err(|e| GuestError::Other(e.to_string()))?;
        let target = match endpoint {
            Endpoint::HvSock { vm_id: id } => {
                let vm_id = Uuid::parse_str(&id)
                    .map_err(|_| GuestError::Other(format!("The hypervisor returned a bad VM id: {id}")))?;
                (self.target)(vm_id)
            }
            Endpoint::Tcp { port } => Target::Loopback(SocketAddr::from(([127, 0, 0, 1], port))),
        };
        Ok((target, key))
    }

    /// A separate connection that carries the live frame stream.
    pub async fn open_stream(&self) -> Result<FrameStream, GuestError> {
        let (target, key) = self.endpoint().await?;
        Ok(FrameStream::connect(&*self.transport, &target, &key, Duration::from_secs(5)).await?)
    }

    async fn client(&self) -> Result<Arc<GuestClient>, GuestError> {
        let mut connection = self.connection.lock().await;
        if let Some(existing) = connection.as_ref() {
            return Ok(existing.clone());
        }
        let (target, key) = self.endpoint().await?;
        let client = Arc::new(GuestClient::connect(&*self.transport, &target, &key, Duration::from_secs(5)).await?);
        *connection = Some(client.clone());
        Ok(client)
    }
}

#[async_trait::async_trait]
impl Guest for LiveGuest {
    async fn call(&self, action: &str, args: BTreeMap<String, String>, timeout: Duration) -> Result<Reply, GuestError> {
        self.upload(action, args, Vec::new(), timeout).await
    }

    async fn upload(
        &self,
        action: &str,
        args: BTreeMap<String, String>,
        payload: Vec<u8>,
        timeout: Duration,
    ) -> Result<Reply, GuestError> {
        for attempt in 0..2 {
            let client = self.client().await?;
            match client.call_with_payload(action, args.clone(), &payload, timeout).await {
                Ok(reply) => return Ok(reply),
                // A dead connection is replaced once; anything else is the agent's real answer.
                Err(ClientError::Proto(_) | ClientError::Connect(_)) if attempt == 0 => {
                    *self.connection.lock().await = None;
                }
                Err(error) => return Err(error.into()),
            }
        }
        unreachable!("the second attempt always returns")
    }

    async fn ctrl_alt_del(&self) -> Result<(), GuestError> {
        let vm = self.handle()?;
        self.require_running(&vm).await?;
        self.hypervisor.press_key(&vm, HostKey::CtrlAltDel).await.map_err(|e| GuestError::Other(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use tw_core::machine::VmOptions;
    use tw_core::{Machine, hv_name};
    use tw_hv::{FakeHypervisor, VmSpec};
    use tw_transport::LoopbackTransport;

    use super::*;
    use crate::testing::{Mode, SECRET, start_agent};

    struct Fixture {
        _dir: tempfile::TempDir,
        guest: LiveGuest,
        hypervisor: Arc<FakeHypervisor>,
        vm: VmHandle,
        address: SocketAddr,
        store: Store,
    }

    async fn fixture(mode: Mode, running: bool) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path()).unwrap();
        let hv_name = hv_name("default", "3fa9c1d2").unwrap();
        let machine = Machine::new("default", &hv_name, VmOptions::default());
        store.save(&machine).unwrap();
        secret::save(&store.directory("default").unwrap().join("channel.secret"), &SECRET).unwrap();

        let hypervisor = Arc::new(FakeHypervisor::default());
        let spec = VmSpec {
            hv_name: hv_name.clone(),
            cpu: 4,
            memory_gib: 8,
            disk_gib: 64,
            width: 1280,
            height: 800,
            network: true,
            directory: dir.path().to_path_buf(),
        };
        let created = tw_hv::Hypervisor::create(&*hypervisor, &spec).await.unwrap();
        if running {
            tw_hv::Hypervisor::start(&*hypervisor, &created.handle).await.unwrap();
        }
        let address = start_agent(mode).await;
        let guest =
            LiveGuest::new(store.clone(), "default", hypervisor.clone(), Arc::new(LoopbackTransport), Uuid::nil())
                .with_target(move |_| Target::Loopback(address));
        Fixture { _dir: dir, guest, hypervisor, vm: created.handle, address, store }
    }

    fn status() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    #[tokio::test]
    async fn calls_reach_the_agent_and_the_connection_is_reused() {
        let f = fixture(Mode::Normal, true).await;
        for _ in 0..3 {
            let reply = f.guest.call("status", status(), Duration::from_secs(2)).await.unwrap();
            assert_eq!(reply.values["session"], "ready");
        }
    }

    #[tokio::test]
    async fn a_forwarded_tcp_port_is_used_when_the_backend_reports_one() {
        let f = fixture(Mode::Normal, true).await;
        f.hypervisor.use_tcp_port(f.address.port());
        // No target override: the port from the hypervisor decides.
        let guest =
            LiveGuest::new(f.store.clone(), "default", f.hypervisor.clone(), Arc::new(LoopbackTransport), Uuid::nil());
        let reply = guest.call("status", status(), Duration::from_secs(2)).await.unwrap();
        assert_eq!(reply.values["session"], "ready");
    }

    #[tokio::test]
    async fn a_stopped_vm_is_reported_before_any_connection() {
        let f = fixture(Mode::Normal, false).await;
        let error = f.guest.call("status", status(), Duration::from_secs(1)).await.unwrap_err();
        assert!(matches!(error, GuestError::NotRunning { ref state, .. } if state == "stopped"), "{error:?}");
        assert!(error.explain("default").contains("terminal-windows start"));
    }

    #[tokio::test]
    async fn a_missing_machine_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let guest = LiveGuest::new(
            Store::at(dir.path()).unwrap(),
            "ghost",
            Arc::new(FakeHypervisor::default()),
            Arc::new(LoopbackTransport),
            Uuid::nil(),
        );
        let error = guest.call("status", status(), Duration::from_secs(1)).await.unwrap_err();
        assert!(matches!(error, GuestError::NotCreated(_)));
    }

    #[tokio::test]
    async fn a_dropped_connection_is_replaced_once() {
        let f = fixture(Mode::HangUpAfterOne, true).await;
        for _ in 0..3 {
            f.guest.call("status", status(), Duration::from_secs(2)).await.unwrap();
        }
    }

    #[tokio::test]
    async fn agent_errors_are_not_retried() {
        let f = fixture(Mode::Normal, true).await;
        let args = BTreeMap::from([("x".to_string(), "99999".to_string()), ("y".to_string(), "0".to_string())]);
        let error = f.guest.call("click", args, Duration::from_secs(2)).await.unwrap_err();
        assert!(matches!(error, GuestError::Client(ClientError::Remote { .. })), "{error:?}");
    }

    #[tokio::test]
    async fn ctrl_alt_del_goes_through_the_hypervisor() {
        let f = fixture(Mode::Normal, true).await;
        f.guest.ctrl_alt_del().await.unwrap();
        assert_eq!(f.hypervisor.keys(&f.vm), [HostKey::CtrlAltDel]);
    }

    #[tokio::test]
    async fn ctrl_alt_del_needs_a_running_vm() {
        let f = fixture(Mode::Normal, false).await;
        assert!(matches!(f.guest.ctrl_alt_del().await, Err(GuestError::NotRunning { .. })));
    }
}
