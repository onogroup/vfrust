pub(crate) mod bootloader;
pub(crate) mod config_builder;
pub(crate) mod delegate;
pub(crate) mod device;
pub(crate) mod machine;
pub(crate) mod process_info;
pub(crate) mod vmnet;

use objc2_foundation::{NSError, NSString, NSURL};
use objc2::rc::Retained;
use std::path::Path;

use crate::error::{Error, VzErrorCode};

pub(crate) fn ns_error_to_error(err: &NSError) -> Error {
    let code = err.code();
    let vz_code = VzErrorCode::from_ns_code(code);
    let mut message = err.localizedDescription().to_string();

    // Capture additional diagnostics from NSError.
    if let Some(reason) = err.localizedFailureReason() {
        message.push_str(&format!(" Reason: {reason}"));
    }
    if let Some(recovery) = err.localizedRecoverySuggestion() {
        message.push_str(&format!(" Recovery: {recovery}"));
    }

    // Dump userInfo keys for debugging opaque errors.
    let user_info = err.userInfo();
    let keys = user_info.allKeys();
    if !keys.is_empty() {
        let key_strs: Vec<String> = keys
            .iter()
            .map(|k| format!("{}", k))
            .collect();
        message.push_str(&format!(" userInfo keys: [{}]", key_strs.join(", ")));
    }

    // Try to get underlying error description via a safe downcast.
    // Safety: NSUnderlyingErrorKey is an extern static from Foundation; accessing it is safe
    // in practice but requires an unsafe block per Rust's extern static rules.
    {
        use objc2_foundation::NSUnderlyingErrorKey;
        let key = unsafe { NSUnderlyingErrorKey };
        if let Some(inner_obj) = user_info.objectForKey(key) {
            if let Ok(inner_err) = Retained::downcast::<NSError>(inner_obj) {
                let desc = inner_err.localizedDescription().to_string();
                message.push_str(&format!(" underlying: {desc}"));
            }
        }
    }

    Error::VzError {
        code: vz_code,
        message,
    }
}

pub(crate) fn nsurl_from_path(path: &Path) -> crate::Result<Retained<NSURL>> {
    let path_str = path
        .to_str()
        .ok_or_else(|| Error::InvalidConfiguration("path contains invalid UTF-8".into()))?;
    let ns_string = NSString::from_str(path_str);
    let url = NSURL::fileURLWithPath(&ns_string);
    Ok(url)
}
