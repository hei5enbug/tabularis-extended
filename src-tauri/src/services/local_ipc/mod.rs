mod endpoint;
mod gui;
mod platform;
mod wire;
pub use endpoint::{EndpointKind, InstanceMetadata, LocalIpc, LocalIpcRouter, RuntimeRouter};
pub use gui::{GuiApplyAck, GuiApplyRequest, PendingGuiAcks};
pub use platform::{current_local_caller, VerifiedPeer};

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "../tests/support.rs"]
#[allow(dead_code)]
mod test_support;

#[cfg(test)]
mod child_fixture;
#[cfg(test)]
fn module_name() -> &'static str {
    module_path!()
        .split_once("::")
        .expect("crate-qualified module")
        .1
}
