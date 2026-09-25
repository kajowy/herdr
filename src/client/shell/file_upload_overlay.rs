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
    // The destination is only choosable before a transfer starts; once it is running or done,
    // `tab` no longer does anything (`cycle_file_upload_destination` refuses while running), so
    // the picker and its hint drop out and the plain reported destination takes over.
    let choosing = !c.running && !c.done;
    // The typed-path destination needs one extra line for its text input, only while it is the
    // selected destination and still choosable.
    let editing_path = choosing
        && matches!(
            c.destination,
            crate::api::schema::FilePutDestination::HomePath
        );
    let height = 9 + 1 + u16::from(editing_path) + u16::from(choosing);
    let q = popup(b.area, 72, height)?;
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
    put_text(
        b,
        i.x,
        i.y + 1,
        i.width,
        &format!(" {manifest}"),
        Style::default().fg(p.text).bg(p.panel_bg),
    );

    if choosing {
        render_destination_choices(b, Rect::new(i.x, i.y + 2, i.width, 1), c, p);
    } else {
        // Once the server has answered a begin it has told us where it is actually writing,
        // which may be a configured inbox rather than the default. Before that, name the
        // destination kind the transfer is already running against.
        let destination = if c.destination_label.is_empty() {
            destination_kind_label(c.destination, &c.home_path_input)
        } else {
            c.destination_label.clone()
        };
        put_text(
            b,
            i.x,
            i.y + 2,
            i.width,
            &format!(" → {destination}"),
            Style::default().fg(p.text).bg(p.panel_bg),
        );
    }

    let mut line = i.y + 3;
    let mut cursor = None;
    if editing_path {
        let input = Rect::new(i.x + 1, line, i.width.saturating_sub(2), 1);
        b.set_style(input, Style::default().fg(p.text).bg(p.surface0));
        cursor = text_editor::render(
            b,
            input,
            &c.home_path_input,
            Style::default().fg(p.text).bg(p.surface0),
        );
        if c.home_path_input.as_str().is_empty() {
            // The caret `text_editor::render` placed sits in the first cell; the placeholder
            // starts right after it so the cursor stays visible over blank input.
            put_text(
                b,
                input.x + 1,
                input.y,
                input.width.saturating_sub(1),
                "path from ~ (home on the receiving machine), e.g. downloads — required",
                Style::default().fg(p.overlay0).bg(p.surface0),
            );
        }
        line += 1;
    }
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

    if choosing {
        put_text(
            b,
            i.x,
            i.bottom().saturating_sub(2),
            i.width,
            " tab destination",
            Style::default().fg(p.overlay1).bg(p.panel_bg),
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
        cursor,
        ..OverlayRender::default()
    })
}

/// The kind's own name for a destination not yet confirmed by the server. `HomePath` shows what
/// has been typed so far, or a placeholder marker while nothing has.
fn destination_kind_label(
    dest: crate::api::schema::FilePutDestination,
    typed: &TextEditor,
) -> String {
    match dest {
        crate::api::schema::FilePutDestination::Inbox => "herdr-inbox".to_owned(),
        crate::api::schema::FilePutDestination::PaneCwd => "this pane's directory".to_owned(),
        crate::api::schema::FilePutDestination::HomePath => {
            let typed = typed.trim();
            if typed.is_empty() {
                "~/…".to_owned()
            } else {
                format!("~/{typed}")
            }
        }
    }
}

/// One row with all three destinations laid out as pills, the selected one highlighted, so the
/// destination reads as a `tab`-cycled choice rather than a fixed label.
fn render_destination_choices(
    b: &mut Buffer,
    area: Rect,
    c: &ClientFileUploadOverlay,
    p: &Palette,
) {
    use crate::api::schema::FilePutDestination;
    let options = [
        FilePutDestination::PaneCwd,
        FilePutDestination::Inbox,
        FilePutDestination::HomePath,
    ];
    let mut x = area.x;
    for option in options {
        if x >= area.right() {
            break;
        }
        let label = format!(" {} ", destination_kind_label(option, &c.home_path_input));
        let active = option == c.destination;
        let style = if active {
            Style::default()
                .fg(contrast(p))
                .bg(p.blue)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.overlay0).bg(p.panel_bg)
        };
        let width = display_width(&label).min(area.right().saturating_sub(x));
        put_text(b, x, area.y, width, &label, style);
        x += width;
    }
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
