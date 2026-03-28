pub(crate) mod bootloader;
pub(crate) mod config_builder;
pub(crate) mod delegate;
pub(crate) mod device;
pub(crate) mod machine;

use objc2_foundation::{NSError, NSString, NSURL};
use objc2::rc::Retained;
use std::path::Path;

use crate::error::{Error, VzErrorCode};

pub(crate) fn ns_error_to_error(err: &NSError) -> Error {
    let code = err.code();
    let vz_code = VzErrorCode::from_ns_code(code);
    let message = err.localizedDescription().to_string();
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
