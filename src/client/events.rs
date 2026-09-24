use super::*;

/// Internal events for the client event loop.
pub(super) enum ClientLoopEvent {
    #[cfg(unix)]
    StdinInput(Vec<u8>),
    #[cfg(unix)]
    PixelMouse(Vec<u8>, crate::input::mouse::HostGeometry),
    #[cfg(unix)]
    DirectGraphicsResponse(direct_graphics::Response),
    #[cfg(windows)]
    StdinEvents(Vec<crate::protocol::ClientInputEvent>),
    Resize(u16, u16, u32, u32, bool),
    TerminalUnavailable(io::Error),
    ServerMessage {
        endpoint_id: endpoint::ClientEndpointId,
        generation: u64,
        message: Box<ServerMessage>,
    },
    ServerDisconnected {
        endpoint_id: endpoint::ClientEndpointId,
        generation: u64,
    },
    EndpointSupervisor(endpoint::EndpointSupervisorEvent),
    EndpointCatalog(Result<Vec<endpoint::SavedSshEndpoint>, String>),
    ActivateEndpoint {
        endpoint_id: endpoint::ClientEndpointId,
        target: Option<shell::ClientEndpointFocusTarget>,
        /// A superseded handoff deliberately starts a fresh target-on epoch even when source and
        /// latest target have the same identity after restoration.
        force: bool,
    },
    Timer,
    /// A file chooser selection. Only sent for a real selection: `paths` is always non-empty,
    /// since a cancel sends no event at all and a failure sends `FileChooserFailed` instead.
    FileChooserResult {
        paths: Vec<std::path::PathBuf>,
        /// True when the AppleScript fallback ran, so directories were not offered.
        files_only: bool,
    },
    /// The file chooser could not run, or failed for a reason that was not a cancel. Distinct
    /// from a cancel (which sends no event) so the failure can be surfaced to the user instead
    /// of being silently indistinguishable from "nothing was selected".
    FileChooserFailed,
}
