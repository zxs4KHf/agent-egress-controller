//! In-memory stand-in for the machine, used by unit tests here and by later Controller tests.

use std::collections::{HashMap, HashSet};

use crate::model::{
    proxy_flags, ListenerOwner, ProxySettings, RegistryProxy, StartupItem, TimezoneState,
};
use crate::probe::{ProbeError, ProbeResult, SystemProxyReader, TimezoneReader, WriterEvidence};

#[derive(Debug, Clone)]
pub struct FakeHost {
    pub proxy: ProxySettings,
    pub registry: RegistryProxy,
    pub timezone: TimezoneState,
    pub mutexes: HashSet<String>,
    pub startup: Vec<StartupItem>,
    pub listeners: HashMap<u16, ListenerOwner>,
    pub fail_proxy: Option<ProbeError>,
    pub fail_timezone: Option<ProbeError>,
    pub fail_mutex: Option<ProbeError>,
    pub fail_startup: Option<ProbeError>,
}

impl Default for FakeHost {
    fn default() -> Self {
        Self {
            proxy: ProxySettings {
                flags: proxy_flags::DIRECT,
                proxy_server: None,
                bypass: None,
                auto_config_url: None,
            },
            registry: RegistryProxy::default(),
            timezone: TimezoneState {
                windows_id: "China Standard Time".into(),
                dynamic_daylight_disabled: false,
            },
            mutexes: HashSet::new(),
            startup: Vec::new(),
            listeners: HashMap::new(),
            fail_proxy: None,
            fail_timezone: None,
            fail_mutex: None,
            fail_startup: None,
        }
    }
}

fn fail_or<T>(fail: &Option<ProbeError>, ok: impl FnOnce() -> T) -> ProbeResult<T> {
    match fail {
        Some(e) => Err(e.clone()),
        None => Ok(ok()),
    }
}

impl SystemProxyReader for FakeHost {
    fn read(&self) -> ProbeResult<ProxySettings> {
        fail_or(&self.fail_proxy, || self.proxy.clone())
    }
    fn read_registry(&self) -> ProbeResult<RegistryProxy> {
        fail_or(&self.fail_proxy, || self.registry.clone())
    }
}

impl TimezoneReader for FakeHost {
    fn read(&self) -> ProbeResult<TimezoneState> {
        fail_or(&self.fail_timezone, || self.timezone.clone())
    }
}

impl WriterEvidence for FakeHost {
    fn mutex_exists(&self, name: &str) -> ProbeResult<bool> {
        fail_or(&self.fail_mutex, || self.mutexes.contains(name))
    }
    fn startup_items(&self) -> ProbeResult<Vec<StartupItem>> {
        fail_or(&self.fail_startup, || self.startup.clone())
    }
    fn listener_owner(&self, port: u16) -> ProbeResult<Option<ListenerOwner>> {
        Ok(self.listeners.get(&port).cloned())
    }
}
