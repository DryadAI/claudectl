//! State routing for an opt-in graph execution profile. Never changes HOME.
use std::path::PathBuf;

pub fn active() -> bool {
    std::env::var_os("CLAUDECTL_PROFILE").is_some()
}

pub fn home() -> Option<std::ffi::OsString> {
    std::env::var_os("CLAUDECTL_PROFILE").or_else(|| std::env::var_os("HOME"))
}

pub fn root() -> Option<PathBuf> {
    std::env::var_os("CLAUDECTL_PROFILE").map(PathBuf::from)
}
