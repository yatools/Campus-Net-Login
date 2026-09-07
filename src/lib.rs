#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(not(windows))]
compile_error!("CampusNetAutoLogin only supports Windows.");

pub mod credentials;
pub mod monitor;
pub mod network;
pub mod settings;
pub mod startup;
pub mod time_utils;
pub mod ui;
pub mod wide;

pub type AppResult<T> = Result<T, String>;
