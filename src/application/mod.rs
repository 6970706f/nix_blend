//! Use cases of Nixblend. They orchestrate the domain through the interfaces in
//! [`ports`], without calling tools or touching files themselves.

pub mod error;
pub mod get_package_info;
pub mod install_package;
pub mod list_packages;
mod managed;
pub mod ports;
mod rebuild_step;
pub mod reinstall_package;
pub mod remove_package;
pub mod search_packages;

#[cfg(test)]
mod test_support;
