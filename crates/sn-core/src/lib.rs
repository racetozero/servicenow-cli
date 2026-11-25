//! ServiceNow profiles, sign-in and HTTP client. No terminal code lives here.
#![forbid(unsafe_code)]

pub mod catalog;
pub mod client;
pub mod config;
pub mod error;
pub mod jwt;
pub mod oauth;
pub mod percent;
pub mod query;
pub mod refs;
pub mod table;
pub mod uisession;

pub use client::{Client, Method, Request, Response};
pub use error::{Choice, Error, Result};
pub use table::{DisplayValue, Page, TableQuery};

/// Seconds since 1970.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// True for a 32-digit lowercase or uppercase hexadecimal record key.
pub fn is_sys_id(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|b| b.is_ascii_hexdigit())
}
