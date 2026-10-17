//! Serve the settings page with an in-memory backend, for looking at it in a browser
//! (screenshots, layout work). Nothing is written to the machine.
//!
//! cargo run -p aec-core --example setup_preview

use std::sync::{Arc, Mutex};

use aec_core::profile::{FieldError, ProfileSet};
use aec_core::setup::{self, CredentialChange, ExitForm, SetupBackend, SetupServer, SetupView};

struct Memory {
    set: ProfileSet,
    user: Option<String>,
    tunnel_stored: bool,
    takeover: bool,
    settings: aec_core::controller::Settings,
}

struct Preview(Mutex<Memory>);

impl SetupBackend for Preview {
    fn view(&self) -> SetupView {
        let g = self.0.lock().unwrap();
        setup::view(
            &g.set,
            &setup::ViewInput {
                core_mixed_port: g.settings.core_mixed_port,
                stored_user: g.user.as_deref(),
                tunnel_password_stored: g.tunnel_stored,
                takeover_enabled: g.takeover,
                client_cooperation: false,
                locked: false,
            },
        )
    }
    fn save(&self, form: ExitForm) -> Result<(), Vec<FieldError>> {
        let mut g = self.0.lock().unwrap();
        let settings = setup::apply_settings(&form, g.settings)?;
        let (user, stored) = (g.user.clone(), g.tunnel_stored);
        let a = setup::apply(&mut g.set, &form, user.as_deref(), stored)?;
        if let CredentialChange::Store { ref user, .. } = a.exit {
            g.user = Some(user.clone());
        }
        if matches!(a.tunnel, CredentialChange::Store { .. }) {
            g.tunnel_stored = true;
        }
        g.takeover = a.takeover;
        g.settings = settings;
        println!("saved: exit {:?}, tunnel {:?}", a.exit, a.tunnel);
        Ok(())
    }
}

fn main() {
    let backend = Arc::new(Preview(Mutex::new(Memory {
        set: ProfileSet::default(),
        user: None,
        tunnel_stored: false,
        takeover: false,
        settings: Default::default(),
    })));
    let srv = SetupServer::start(0, "preview".into(), backend).expect("listen");
    println!("{}", srv.url());
    loop {
        std::thread::park();
    }
}
