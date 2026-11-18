// ============================================================================
// Social app scanner module
// ============================================================================
//
// core only handles the result model, unified traversal, and classification; each app file handles path discovery, avoiding
// a single scanner from continuously bloating, and making it easier to independently fix missed-scan issues for different client versions.
// ============================================================================

mod common;
mod core;
mod dingtalk;
mod feishu;
mod line;
mod ntqq;
mod qq;
mod telegram;
mod wechat;
mod whatsapp;
mod wxwork;

pub use core::*;
