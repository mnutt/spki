//! Session-scoped browser authority, compatible with the isolate author SDK bridge.
use capnp::capability::Client;
use isolate_protocol::{
    grain_capnp::{sandstorm_api, session_context},
    isolate_bridge_capnp::{browser_isolate_bridge, isolate_bridge},
};
use std::{
    cell::RefCell,
    collections::HashMap,
    rc::{Rc, Weak},
};

pub struct State {
    pub api: RefCell<Option<sandstorm_api::Client<capnp::any_pointer::Owned>>>,
    sessions: RefCell<HashMap<String, Weak<Session>>>,
}
pub struct Session {
    pub id: String,
    pub context: session_context::Client,
    pub closed: tokio::sync::watch::Sender<()>,
    pub socket_slots: std::sync::Arc<tokio::sync::Semaphore>,
    state: Weak<State>,
}
impl Drop for Session {
    fn drop(&mut self) {
        if let Some(state) = self.state.upgrade() {
            state.sessions.borrow_mut().remove(&self.id);
        }
    }
}
pub struct Worker {
    pub state: Rc<State>,
    handoffs: RefCell<HashMap<String, (Weak<Session>, Client)>>,
    pub stopped: tokio::sync::watch::Sender<bool>,
    pub socket_slots: std::sync::Arc<tokio::sync::Semaphore>,
}
fn error(message: &str) -> capnp::Error {
    capnp::Error::failed(message.into())
}
pub fn random_id() -> capnp::Result<String> {
    let mut bytes = [0; 24];
    getrandom::fill(&mut bytes).map_err(|_| error("could not generate session ID"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
impl State {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            api: RefCell::new(None),
            sessions: Default::default(),
        })
    }
    pub fn session(
        self: &Rc<Self>,
        context: session_context::Client,
    ) -> capnp::Result<Rc<Session>> {
        self.sessions
            .borrow_mut()
            .retain(|_, value| value.strong_count() > 0);
        if self.sessions.borrow().len() >= 256 {
            return Err(error("too many sessions"));
        }
        let session = Rc::new(Session {
            id: random_id()?,
            context,
            closed: tokio::sync::watch::channel(()).0,
            socket_slots: std::sync::Arc::new(tokio::sync::Semaphore::new(16)),
            state: Rc::downgrade(self),
        });
        self.sessions
            .borrow_mut()
            .insert(session.id.clone(), Rc::downgrade(&session));
        Ok(session)
    }
    fn get(&self, id: &str) -> capnp::Result<Rc<Session>> {
        self.sessions
            .borrow()
            .get(id)
            .and_then(Weak::upgrade)
            .ok_or_else(|| error("session is no longer available"))
    }
}
impl Worker {
    pub fn new(state: Rc<State>) -> Rc<Self> {
        Rc::new(Self {
            state,
            handoffs: Default::default(),
            stopped: tokio::sync::watch::channel(false).0,
            socket_slots: std::sync::Arc::new(tokio::sync::Semaphore::new(64)),
        })
    }
    pub fn release_session(&self, id: &str) {
        self.handoffs
            .borrow_mut()
            .retain(|_, (session, _)| session.upgrade().is_some_and(|session| session.id != id));
    }
    pub fn stop(&self) {
        self.stopped.send_replace(true);
        self.handoffs.borrow_mut().clear();
    }
    fn live(&self) -> capnp::Result<()> {
        if *self.stopped.borrow() {
            Err(error("worker replaced"))
        } else {
            Ok(())
        }
    }
}
impl isolate_bridge::Server for Worker {
    async fn get_sandstorm_api(
        self: Rc<Self>,
        _: isolate_bridge::GetSandstormApiParams,
        mut results: isolate_bridge::GetSandstormApiResults,
    ) -> capnp::Result<()> {
        self.live()?;
        results.get().set_api(
            self.state
                .api
                .borrow()
                .clone()
                .ok_or_else(|| error("Sandstorm API unavailable"))?,
        );
        Ok(())
    }
    async fn get_session_context(
        self: Rc<Self>,
        params: isolate_bridge::GetSessionContextParams,
        mut results: isolate_bridge::GetSessionContextResults,
    ) -> capnp::Result<()> {
        self.live()?;
        let session = self.state.get(params.get()?.get_session_id()?.to_str()?)?;
        results.get().set_context(session.context.clone());
        Ok(())
    }
    async fn create_browser_handoff(
        self: Rc<Self>,
        params: isolate_bridge::CreateBrowserHandoffParams,
        mut results: isolate_bridge::CreateBrowserHandoffResults,
    ) -> capnp::Result<()> {
        self.live()?;
        let params = params.get()?;
        let session = self.state.get(params.get_session_id()?.to_str()?)?;
        let mut handoffs = self.handoffs.borrow_mut();
        handoffs.retain(|_, (session, _)| session.strong_count() > 0);
        if handoffs.len() >= 1024 {
            return Err(error("too many browser handoffs"));
        }
        let id = random_id()?;
        handoffs.insert(
            id.clone(),
            (
                Rc::downgrade(&session),
                params.get_cap().get_as_capability()?,
            ),
        );
        results.get().set_id(id);
        Ok(())
    }
    async fn drop_browser_handoff(
        self: Rc<Self>,
        params: isolate_bridge::DropBrowserHandoffParams,
        mut results: isolate_bridge::DropBrowserHandoffResults,
    ) -> capnp::Result<()> {
        results.get().set_released(
            self.handoffs
                .borrow_mut()
                .remove(params.get()?.get_id()?.to_str()?)
                .is_some(),
        );
        Ok(())
    }
}
pub struct Application {
    pub runtime: Weak<crate::Runtime>,
    pub headers: Vec<(String, String)>,
    pub capability: tokio::sync::OnceCell<Client>,
}
pub struct Browser {
    pub application: Option<Application>,
    pub worker: Rc<Worker>,
    pub session: Weak<Session>,
}
impl browser_isolate_bridge::Server for Browser {
    async fn get_application(
        self: Rc<Self>,
        _: browser_isolate_bridge::GetApplicationParams,
        mut results: browser_isolate_bridge::GetApplicationResults,
    ) -> capnp::Result<()> {
        self.worker.live()?;
        let session = self
            .session
            .upgrade()
            .ok_or_else(|| error("browser session closed"))?;
        let application = self
            .application
            .as_ref()
            .ok_or_else(|| error("application unavailable"))?;
        let cap = application
            .capability
            .get_or_try_init(|| async {
                let runtime = application
                    .runtime
                    .upgrade()
                    .ok_or_else(|| error("worker replaced"))?;
                let response = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    runtime.fetch_with_headers(
                        isolate_protocol::http_over_capnp_capnp::HttpMethod::Post,
                        "__spki_internal_connect",
                        &[],
                        &application.headers,
                    ),
                )
                .await
                .map_err(|_| error("application bootstrap timed out"))?
                .map_err(|_| error("application bootstrap failed"))?;
                if response.status != 200 {
                    return Err(error("application connect() failed or is not defined"));
                }
                let handoff: serde_json::Value = serde_json::from_slice(&response.body)
                    .map_err(|_| error("invalid application bootstrap"))?;
                let id = handoff["id"]
                    .as_str()
                    .ok_or_else(|| error("missing application capability"))?;
                self.worker.live()?;
                let (owner, cap) = self
                    .worker
                    .handoffs
                    .borrow_mut()
                    .remove(id)
                    .ok_or_else(|| error("application capability unavailable"))?;
                if !owner
                    .upgrade()
                    .is_some_and(|owner| Rc::ptr_eq(&owner, &session))
                {
                    return Err(error("application capability belongs to another session"));
                }
                Ok(cap)
            })
            .await?;
        results
            .get()
            .get_cap()
            .set_as_capability(cap.hook.add_ref());
        Ok(())
    }

    async fn get_handoff_capability(
        self: Rc<Self>,
        params: browser_isolate_bridge::GetHandoffCapabilityParams,
        mut results: browser_isolate_bridge::GetHandoffCapabilityResults,
    ) -> capnp::Result<()> {
        self.worker.live()?;
        let session = self
            .session
            .upgrade()
            .ok_or_else(|| error("browser session closed"))?;
        let handoffs = self.worker.handoffs.borrow();
        let (owner, cap) = handoffs
            .get(params.get()?.get_id()?.to_str()?)
            .ok_or_else(|| error("handoff not found"))?;
        if !owner
            .upgrade()
            .is_some_and(|owner| Rc::ptr_eq(&owner, &session))
        {
            return Err(error("handoff belongs to another session"));
        }
        results
            .get()
            .get_cap()
            .set_as_capability(cap.hook.add_ref());
        Ok(())
    }
    async fn claim_powerbox_request(
        self: Rc<Self>,
        params: browser_isolate_bridge::ClaimPowerboxRequestParams,
        mut results: browser_isolate_bridge::ClaimPowerboxRequestResults,
    ) -> capnp::Result<()> {
        self.worker.live()?;
        let session = self
            .session
            .upgrade()
            .ok_or_else(|| error("browser session closed"))?;
        let params = params.get()?;
        // Authoring configuration currently declares no application permissions. Never map
        // Preview's deploy bit into app permissions, or silently ignore required permissions.
        if !params.get_required_permissions()?.is_empty() {
            return Err(error("application permissions are not declared"));
        }
        let token = params.get_request_token()?;
        if token.len() > 4096 {
            return Err(error("request token too long"));
        }
        let mut request = session.context.claim_request_request();
        request.get().set_request_token(token);
        let response = request.send().promise.await?;
        results.get().get_cap().set_as(response.get()?.get_cap())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Context;
    impl session_context::Server for Context {}
    struct Handle;
    impl isolate_protocol::util_capnp::handle::Server for Handle {}
    #[test]
    fn browser_handoffs_are_session_scoped_and_revoked_on_replacement() {
        futures::executor::block_on(async {
            let state = State::new();
            let a = state.session(capnp_rpc::new_client(Context)).unwrap();
            let b = state.session(capnp_rpc::new_client(Context)).unwrap();
            let worker = Worker::new(state);
            let native: isolate_bridge::Client = capnp_rpc::new_client_from_rc(worker.clone());
            let cap: isolate_protocol::util_capnp::handle::Client = capnp_rpc::new_client(Handle);
            use capnp::capability::FromClientHook;
            let mut create = native.create_browser_handoff_request();
            create.get().set_session_id(&a.id);
            create
                .get()
                .get_cap()
                .set_as_capability(cap.into_client_hook());
            let result = create.send().promise.await.unwrap();
            let id = result.get().unwrap().get_id().unwrap();
            let browser: browser_isolate_bridge::Client = capnp_rpc::new_client(Browser {
                application: None,
                worker: worker.clone(),
                session: Rc::downgrade(&a),
            });
            let other: browser_isolate_bridge::Client = capnp_rpc::new_client(Browser {
                application: None,
                worker: worker.clone(),
                session: Rc::downgrade(&b),
            });
            let mut get = other.get_handoff_capability_request();
            get.get().set_id(id);
            assert!(get.send().promise.await.is_err());
            let mut get = browser.get_handoff_capability_request();
            get.get().set_id(id);
            assert!(get.send().promise.await.is_ok());
            worker.stop();
            let mut get = browser.get_handoff_capability_request();
            get.get().set_id(id);
            assert!(get.send().promise.await.is_err());
            assert!(worker.handoffs.borrow().is_empty());
        });
    }
}
