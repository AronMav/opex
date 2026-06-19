// Module 1 Task 1: OAuth foundation types.
// Constants are used by later tasks (Modules 2–4); suppress dead_code until wired up.
#![allow(dead_code)]

pub mod types;

// OAuth endpoint constants.
pub const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
pub const DEVICE_CODE_ENDPOINT: &str = "https://oauth2.googleapis.com/device/code";
pub const USERINFO_ENDPOINT: &str = "https://www.googleapis.com/oauth2/v1/userinfo";
pub const OAUTH_SCOPES: &str = "https://www.googleapis.com/auth/cloud-platform \
    https://www.googleapis.com/auth/userinfo.email \
    https://www.googleapis.com/auth/userinfo.profile";
pub const REDIRECT_HOST: &str = "127.0.0.1";
pub const DEFAULT_REDIRECT_PORT: u16 = 8085;
pub const CALLBACK_PATH: &str = "/oauth2callback";
pub const REFRESH_SKEW_SECONDS: i64 = 60;
