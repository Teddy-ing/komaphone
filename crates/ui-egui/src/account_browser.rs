//! OAuth URLs are confidential effects, never ordinary links or status text.
use crate::{OpenUrlFn, PhotocraftApp};

const OPEN_FAILED: &str = "Could not open Google sign-in in your browser. Check your default browser, then try signing in again. Local drawings are unchanged.";

pub(crate) fn open(app: &mut PhotocraftApp, url: &str) {
    let result = handoff(app.services.open_url.as_ref(), url, || {
        // Cancel through the same engine command as the account UI. Neither the
        // authorization URL nor the platform error enters the command seam.
        let _ = app.session.execute("komaphone.account.cancel", serde_json::json!({}));
    });
    if let Err(message) = result {
        app.ui.status = message.into();
    }
}

fn handoff(open_url: Option<&OpenUrlFn>, url: &str, cancel: impl FnOnce()) -> Result<(), &'static str> {
    if open_url.is_some_and(|open| open(url).is_ok()) {
        Ok(())
    } else {
        cancel();
        Err(OPEN_FAILED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    const PRIVATE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth?state=private-state-marker&nonce=private-nonce-marker";

    #[test]
    fn successful_handoff_uses_platform_service_without_status_or_journal_exposure() {
        let opened = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&opened);
        let services = crate::Services {
            open_url: Some(Box::new(move |url| {
                observed.lock().unwrap().push(url.to_owned());
                Ok(())
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        app.ui.status = "Saved local drawing".into();
        open(&mut app, PRIVATE_URL);
        assert_eq!(*opened.lock().unwrap(), [PRIVATE_URL]);
        assert_eq!(app.ui.status, "Saved local drawing");
        assert!(app.session.journal.is_empty());
    }

    #[test]
    fn failed_or_missing_service_cancels_and_returns_only_static_diagnostic() {
        let failing: OpenUrlFn = Box::new(|url| Err(format!("private-launcher-error: {url}")));
        for service in [Some(&failing), None] {
            let mut canceled = false;
            let message = handoff(service, PRIVATE_URL, || canceled = true).unwrap_err();
            assert!(canceled);
            assert_eq!(message, OPEN_FAILED);
            assert!(!message.contains("private-"));
        }
    }

    #[test]
    fn application_failure_status_and_cancel_journal_do_not_reflect_browser_input() {
        for fail_service in [false, true] {
            let mut services = crate::Services::default();
            if fail_service {
                services.open_url = Some(Box::new(|url| Err(format!("private-launcher-error: {url}"))));
            }
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
            open(&mut app, PRIVATE_URL);
            assert_eq!(app.ui.status, OPEN_FAILED);
            assert!(!serde_json::to_string(&app.session.journal).unwrap().contains("private-"));
        }
    }
}
