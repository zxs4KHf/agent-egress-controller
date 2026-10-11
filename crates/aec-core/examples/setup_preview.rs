//! Serve the settings page with an in-memory backend, for looking at it in a browser
//! (screenshots, layout work). Nothing is written to the machine.
//!
//! cargo run -p aec-core --example setup_preview

use std::sync::{Arc, Mutex};

use aec_core::profile::{FieldError, ProfileSet};
use aec_core::setup::{self, ExitForm, SetupBackend, SetupServer, SetupView};

struct Memory(Mutex<(ProfileSet, Option<String>, bool)>);

impl SetupBackend for Memory {
    fn view(&self) -> SetupView {
        let g = self.0.lock().unwrap();
        setup::view(&g.0, g.1.as_deref(), g.2, false)
    }
    fn save(&self, form: ExitForm) -> Result<(), Vec<FieldError>> {
        let mut g = self.0.lock().unwrap();
        let user = g.1.clone();
        let (change, takeover) = setup::apply(&mut g.0, &form, user.as_deref())?;
        if let setup::CredentialChange::Store { ref user, .. } = change {
            g.1 = Some(user.clone());
        }
        g.2 = takeover;
        println!("saved: {change:?}");
        Ok(())
    }
}

fn main() {
    let backend = Arc::new(Memory(Mutex::new((ProfileSet::default(), None, false))));
    let srv = SetupServer::start(0, "preview".into(), backend).expect("listen");
    println!("{}", srv.url());
    loop {
        std::thread::park();
    }
}
