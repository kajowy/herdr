//! Confirmation dialog, progress display, and cancel control for a file upload: one overlay
//! that becomes the progress view once the transfer starts. See `render_confirm_close_overlay`
//! (`src/client/shell/overlays.rs`) for the shape this follows.

use super::*;
use crate::client::shell::file_upload::ClientFileUploadOverlay;

pub(super) fn render_file_upload_overlay(
    b: &mut Buffer,
    c: &ClientFileUploadOverlay,
    p: &Palette,
) -> Option<OverlayRender> {
    let q = popup(b.area, 72, 9)?;
    let i = panel(b, q, p.blue, p.panel_bg)?;
    put_text(
        b,
        i.x,
        i.y,
        i.width,
        " send files",
        Style::default()
            .fg(p.blue)
            .bg(p.panel_bg)
            .add_modifier(Modifier::BOLD),
    );

    // Counted once in `open_file_upload`: `entries` is immutable afterwards, and every chunk
    // response repaints this overlay.
    let manifest = match (c.file_count, c.directory_count) {
        (files, 0) => format!("{files} file(s), {}", human_bytes(c.total_bytes)),
        (0, dirs) => format!("{dirs} folder(s)"),
        (files, dirs) => {
            format!(
                "{files} file(s) in {dirs} folder(s), {}",
                human_bytes(c.total_bytes)
            )
        }
    };
    // Once the server has answered a begin it has told us where it is actually writing, which may
    // be a configured inbox rather than the default. Before that, name the destination kind.
    let destination = if c.destination_label.is_empty() {
        match c.destination {
            crate::api::schema::FilePutDestination::Inbox => "herdr-inbox",
            crate::api::schema::FilePutDestination::PaneCwd => "this pane's directory",
        }
    } else {
        c.destination_label.as_str()
    };
    put_text(
        b,
        i.x,
        i.y + 1,
        i.width,
        &format!(" {manifest} → {destination}"),
        Style::default().fg(p.text).bg(p.panel_bg),
    );

    let mut line = i.y + 2;
    if c.running || c.done {
        let total = c.entries.len();
        let index = c.index.min(total);
        let percent = c
            .offset
            .min(c.transfer_bytes)
            .saturating_mul(100)
            .checked_div(c.transfer_bytes)
            .unwrap_or(if c.done { 100 } else { 0 });
        let status = if c.done { "done" } else { "sending" };
        put_text(
            b,
            i.x,
            line,
            i.width,
            &format!(" {status} {index}/{total} — {percent}%"),
            Style::default().fg(p.text).bg(p.panel_bg),
        );
        line += 1;
    }

    if let Some(copied) = c.copied.as_deref() {
        put_text(
            b,
            i.x,
            line,
            i.width,
            &format!(" copied {copied}"),
            Style::default().fg(p.text).bg(p.panel_bg),
        );
        line += 1;
    }

    if !c.skipped.is_empty() {
        put_text(
            b,
            i.x,
            line,
            i.width,
            &format!(" {} skipped", c.skipped.len()),
            Style::default().fg(p.overlay0).bg(p.panel_bg),
        );
        line += 1;
    }

    if let Some(error) = c.error.as_deref() {
        put_text(
            b,
            i.x,
            line,
            i.width,
            &format!(" {error}"),
            Style::default().fg(p.red).bg(p.panel_bg),
        );
    }

    let rs = row(i, &[13, 12], 2, i.height.saturating_sub(1));
    let [ok, cancel] = rs.as_slice() else {
        return None;
    };
    button(
        b,
        *ok,
        if c.done { " ↵ close " } else { " ↵ send " },
        Style::default()
            .fg(contrast(p))
            .bg(p.blue)
            .add_modifier(Modifier::BOLD),
    );
    button(
        b,
        *cancel,
        " esc cancel ",
        Style::default()
            .fg(p.text)
            .bg(p.surface0)
            .add_modifier(Modifier::BOLD),
    );
    Some(OverlayRender {
        area: q,
        primary: *ok,
        cancel: *cancel,
        ..OverlayRender::default()
    })
}

/// `B`/`KB`/`MB`/`GB`, one decimal above `KB`.
fn human_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let bytes = bytes as f64;
    if bytes < KB {
        format!("{} B", bytes as u64)
    } else if bytes < MB {
        format!("{:.1} KB", bytes / KB)
    } else if bytes < GB {
        format!("{:.1} MB", bytes / MB)
    } else {
        format!("{:.1} GB", bytes / GB)
    }
}
