use std::collections::{HashMap, HashSet};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use super::*;

pub(super) struct AgentRow {
    pub(super) pane_id: String,
    pub(super) status: crate::api::schema::AgentStatus,
    pub(super) focused: bool,
    pub(super) rows: Vec<Vec<crate::ui::ResolvedToken>>,
}

pub(super) fn ordered_agent_pane_ids(
    snapshot: &ClientShellSnapshot,
    sort: crate::config::AgentPanelSortConfig,
) -> Vec<String> {
    if snapshot.agent_view_label.is_some() {
        return snapshot
            .agent_order
            .iter()
            .filter(|pane_id| {
                snapshot
                    .agents
                    .iter()
                    .any(|agent| agent.pane_id == pane_id.as_str())
            })
            .cloned()
            .collect();
    }
    let mut agents = snapshot.agents.iter().collect::<Vec<_>>();
    if sort == crate::config::AgentPanelSortConfig::Priority {
        agents.sort_by_key(|agent| {
            (
                std::cmp::Reverse(status_priority(agent.agent_status)),
                std::cmp::Reverse(agent.state_change_seq),
            )
        });
    }
    agents
        .into_iter()
        .map(|agent| agent.pane_id.clone())
        .collect()
}

pub(super) fn render_agent_panel(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    agent_scroll: &mut usize,
    hits: &mut ShellHitMap,
) {
    if !render_agent_panel_header(
        buffer,
        area,
        snapshot.agent_view_label.as_deref(),
        config,
        hits,
        true,
    ) {
        return;
    }

    let rows = agent_rows(snapshot, config, None);
    render_agent_list(
        buffer,
        area,
        &rows,
        snapshot
            .agent_view_label
            .as_ref()
            .map(|_| " no matching agents"),
        config,
        agent_scroll,
        hits,
        |row| row.rows.len(),
        |buffer, rect, row, hits| {
            hits.agents.push((rect, row.pane_id.clone()));
            render_agent_row(buffer, rect, row, config);
        },
    );
}

pub(super) fn render_agent_panel_header(
    buffer: &mut Buffer,
    area: Rect,
    agent_view_label: Option<&str>,
    config: &ClientShellConfig,
    hits: &mut ShellHitMap,
    separator: bool,
) -> bool {
    if area.height == 0 {
        return false;
    }
    if separator {
        put_text(
            buffer,
            area.x,
            area.y,
            area.width,
            &"─".repeat(area.width as usize),
            Style::default().fg(config.palette.surface_dim),
        );
    }
    if area.height < 2 {
        return false;
    }
    put_text(
        buffer,
        area.x,
        area.y + 1,
        area.width,
        " agents",
        Style::default()
            .fg(config.palette.overlay0)
            .add_modifier(Modifier::BOLD),
    );
    let sort_label = agent_view_label.unwrap_or(match config.agent_panel_sort {
        crate::config::AgentPanelSortConfig::Spaces => "spaces",
        crate::config::AgentPanelSortConfig::Priority => "priority",
        crate::config::AgentPanelSortConfig::Grouped => "grouped",
    });
    let sort_width = display_width(sort_label).min(area.width as usize) as u16;
    let sort_rect = Rect::new(
        area.right().saturating_sub(sort_width),
        area.y + 1,
        sort_width,
        1,
    );
    hits.agent_sort_toggle = if config.mouse_capture && agent_view_label.is_none() {
        sort_rect
    } else {
        Rect::default()
    };
    put_text(
        buffer,
        sort_rect.x,
        sort_rect.y,
        sort_rect.width,
        sort_label,
        Style::default()
            .fg(if agent_view_label.is_some() {
                config.palette.accent
            } else {
                config.palette.overlay0
            })
            .add_modifier(Modifier::BOLD),
    );
    true
}

pub(super) fn render_agent_list<T>(
    buffer: &mut Buffer,
    area: Rect,
    rows: &[T],
    empty_message: Option<&str>,
    config: &ClientShellConfig,
    agent_scroll: &mut usize,
    hits: &mut ShellHitMap,
    row_lines: impl Fn(&T) -> usize,
    render_row: impl FnMut(&mut Buffer, Rect, &T, &mut ShellHitMap),
) {
    let body = Rect::new(
        area.x,
        area.y.saturating_add(3),
        area.width,
        area.height.saturating_sub(3),
    );
    let row_heights = rows
        .iter()
        .map(|row| row_lines(row).max(1).min(u16::MAX as usize) as u16)
        .collect::<Vec<_>>();
    let gaps = rows
        .iter()
        .enumerate()
        .map(|(index, _)| {
            if index + 1 < rows.len() {
                config.agents.row_gap
            } else {
                0
            }
        })
        .collect::<Vec<_>>();
    render_agent_list_body(
        buffer,
        body,
        rows,
        &row_heights,
        &gaps,
        empty_message,
        config,
        agent_scroll,
        hits,
        render_row,
    );
}

/// Render scrollable agent-panel rows into `body` with per-row heights and trailing gaps.
fn render_agent_list_body<T>(
    buffer: &mut Buffer,
    body: Rect,
    rows: &[T],
    row_heights: &[u16],
    gaps: &[u16],
    empty_message: Option<&str>,
    config: &ClientShellConfig,
    agent_scroll: &mut usize,
    hits: &mut ShellHitMap,
    mut render_row: impl FnMut(&mut Buffer, Rect, &T, &mut ShellHitMap),
) {
    hits.agent_body = body;
    if body.is_empty() || rows.is_empty() {
        *agent_scroll = 0;
        if let Some(message) = empty_message.filter(|_| !body.is_empty()) {
            put_text(
                buffer,
                body.x,
                body.y,
                body.width,
                message,
                Style::default()
                    .fg(config.palette.overlay0)
                    .add_modifier(Modifier::DIM),
            );
        }
        return;
    }

    let metrics = super::scroll::list_scroll_metrics(row_heights, gaps, body.height, *agent_scroll);
    hits.agent_max_scroll = metrics.max_offset_from_bottom;
    hits.agent_scroll_metrics = Some(metrics);
    *agent_scroll = metrics
        .max_offset_from_bottom
        .saturating_sub(metrics.offset_from_bottom);
    let show_scrollbar = metrics.max_offset_from_bottom > 0 && body.width > 1;
    let content_width = body.width.saturating_sub(u16::from(show_scrollbar));
    let mut y = body.y;
    for (index, row) in rows.iter().enumerate().skip(*agent_scroll) {
        let height = row_heights[index].min(body.height);
        if y.saturating_add(height) > body.bottom() {
            break;
        }
        let rect = Rect::new(body.x, y, content_width, height);
        render_row(buffer, rect, row, hits);
        y = y.saturating_add(height).saturating_add(gaps[index]);
    }

    if show_scrollbar {
        let track = Rect::new(body.right().saturating_sub(1), body.y, 1, body.height);
        hits.agent_scrollbar = track;
        super::scroll::render_list_scrollbar(buffer, track, metrics, &config.palette);
    }
}

pub(super) fn agent_rows(
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    machine: Option<&str>,
) -> Vec<AgentRow> {
    ordered_agent_pane_ids(snapshot, config.agent_panel_sort)
        .into_iter()
        .filter_map(|pane_id| agent_row(snapshot, &pane_id, config, machine))
        .collect()
}

pub(super) fn agent_row(
    snapshot: &ClientShellSnapshot,
    pane_id: &str,
    config: &ClientShellConfig,
    machine: Option<&str>,
) -> Option<AgentRow> {
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.pane_id == pane_id)?;
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == agent.workspace_id)?;
    let tab = snapshot.tabs.iter().find(|tab| tab.tab_id == agent.tab_id);
    let pane = snapshot
        .panes
        .iter()
        .find(|pane| pane.pane_id == agent.pane_id);
    let tab_label = agent_tab_label(snapshot, agent, tab);
    let agent_label = agent_display_label(agent);
    let labels = agent
        .state_labels
        .iter()
        .cloned()
        .collect::<HashMap<_, _>>();
    let tokens = agent.tokens.iter().cloned().collect::<HashMap<_, _>>();
    let state_text = labels
        .get(status_text(agent.agent_status))
        .map(String::as_str)
        .unwrap_or_else(|| sidebar_status_text(agent.agent_status));
    let canonical_agent = agent
        .agent
        .as_deref()
        .and_then(crate::detect::parse_agent_label);
    let rows = crate::ui::sidebar_agent_rows(
        &config.agents,
        crate::ui::AgentTokenContext {
            machine,
            workspace: &workspace.label,
            tab: tab_label,
            pane: agent
                .title
                .as_deref()
                .or_else(|| pane.and_then(|pane| pane.label.as_deref())),
            agent_label,
            terminal_title: agent.terminal_title.as_deref(),
            terminal_title_stripped: agent.terminal_title_stripped.as_deref(),
            canonical_agent,
            tokens: &tokens,
        },
        state_text,
    );
    Some(AgentRow {
        pane_id: agent.pane_id.clone(),
        status: agent.agent_status,
        focused: agent.focused,
        rows,
    })
}

/// Tab label worth showing beside an agent: only when its space has several
/// tabs or the tab was named by the user.
fn agent_tab_label<'a>(
    snapshot: &ClientShellSnapshot,
    agent: &crate::protocol::ClientShellAgent,
    tab: Option<&'a crate::protocol::ClientShellTab>,
) -> Option<&'a str> {
    let tab_count = snapshot
        .tabs
        .iter()
        .filter(|candidate| candidate.workspace_id == agent.workspace_id)
        .count();
    tab.filter(|tab| tab_count > 1 || tab.custom_label)
        .map(|tab| tab.label.as_str())
}

fn agent_display_label(agent: &crate::protocol::ClientShellAgent) -> Option<&str> {
    agent
        .display_agent
        .as_deref()
        .or(agent.name.as_deref())
        .or(agent.agent.as_deref())
        .or(agent.title.as_deref())
}

pub(super) fn render_agent_row(
    buffer: &mut Buffer,
    rect: Rect,
    row: &AgentRow,
    config: &ClientShellConfig,
) {
    let palette = &config.palette;
    let row_style = if row.focused {
        Style::default().bg(palette.active_row_bg)
    } else {
        Style::default()
    };
    let name_style = if row.focused {
        Style::default()
            .fg(palette.text)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(palette.subtext0)
            .add_modifier(Modifier::BOLD)
    };
    let status_style = Style::default().fg(status_color(row.status, palette));
    let secondary = Style::default().fg(palette.overlay0);
    let icon = (
        status_icon(row.status, config.status_indicators),
        Style::default().fg(status_color(row.status, palette)),
    );
    let rows = if row.rows.is_empty() {
        vec![vec![crate::ui::ResolvedToken {
            kind: crate::ui::ResolvedTokenKind::StateIcon,
            style: Default::default(),
        }]]
    } else {
        row.rows.clone()
    };
    for (index, tokens) in rows.iter().take(rect.height as usize).enumerate() {
        let indent = if index == 0 { 1 } else { 3 };
        let mut spans = vec![ratatui::text::Span::raw(" ".repeat(indent))];
        spans.extend(crate::ui::resolved_token_spans(
            tokens,
            icon,
            status_style,
            name_style,
            secondary,
            secondary,
            palette,
            rect.width.saturating_sub(indent as u16) as usize,
        ));
        Paragraph::new(Line::from(spans)).style(row_style).render(
            Rect::new(rect.x, rect.y + index as u16, rect.width, 1),
            buffer,
        );
    }
}

/// Collapse key of a space in the grouped agent panel. It shares the client's
/// per-endpoint collapsed-group set and its persistence with worktree groups;
/// the prefix keeps it apart from worktree keys.
pub(super) fn agent_group_key(workspace_id: &str) -> String {
    format!("agents:{workspace_id}")
}

/// Where a run of grouped agent lines comes from.
pub(super) struct GroupedAgentSource<'a> {
    pub(super) endpoint_id: &'a ClientEndpointId,
    /// Machine label prefixed to space headers when several machines are shown.
    pub(super) machine: Option<&'a str>,
    pub(super) stale: bool,
    /// Whether this endpoint's focused agent is the focused agent on screen.
    pub(super) active: bool,
}

pub(super) struct GroupedAgentLine {
    pub(super) endpoint_id: ClientEndpointId,
    pub(super) stale: bool,
    pub(super) kind: GroupedAgentLineKind,
}

pub(super) enum GroupedAgentLineKind {
    /// Space row; followed by its agents unless collapsed.
    Header {
        workspace_id: String,
        label: String,
        agent_count: usize,
        status: crate::api::schema::AgentStatus,
        collapsed: bool,
    },
    Agent {
        pane_id: String,
        status: crate::api::schema::AgentStatus,
        focused: bool,
        text: String,
    },
}

/// Status a space header shows for its agents, most urgent first.
fn grouped_rollup_priority(status: crate::api::schema::AgentStatus) -> u8 {
    use crate::api::schema::AgentStatus;
    match status {
        AgentStatus::Working => 4,
        AgentStatus::Blocked => 3,
        AgentStatus::Done => 2,
        AgentStatus::Idle => 1,
        AgentStatus::Unknown => 0,
    }
}

/// Append one header per space that owns any of `agents` (in space order),
/// followed by its agents unless the space is collapsed.
pub(super) fn push_grouped_agent_lines(
    lines: &mut Vec<GroupedAgentLine>,
    source: GroupedAgentSource<'_>,
    snapshot: &ClientShellSnapshot,
    agents: &[&crate::protocol::ClientShellAgent],
    collapsed_groups: Option<&HashSet<String>>,
) {
    for workspace in &snapshot.workspaces {
        let members = agents
            .iter()
            .filter(|agent| agent.workspace_id == workspace.workspace_id)
            .collect::<Vec<_>>();
        let Some(status) = members
            .iter()
            .map(|agent| agent.agent_status)
            .max_by_key(|status| grouped_rollup_priority(*status))
        else {
            continue;
        };
        let collapsed = collapsed_groups
            .is_some_and(|groups| groups.contains(&agent_group_key(&workspace.workspace_id)));
        lines.push(GroupedAgentLine {
            endpoint_id: source.endpoint_id.clone(),
            stale: source.stale,
            kind: GroupedAgentLineKind::Header {
                workspace_id: workspace.workspace_id.clone(),
                label: source.machine.map_or_else(
                    || workspace.label.clone(),
                    |machine| format!("{machine} · {}", workspace.label),
                ),
                agent_count: members.len(),
                status,
                collapsed,
            },
        });
        if collapsed {
            continue;
        }
        lines.extend(members.into_iter().map(|agent| GroupedAgentLine {
            endpoint_id: source.endpoint_id.clone(),
            stale: source.stale,
            kind: GroupedAgentLineKind::Agent {
                pane_id: agent.pane_id.clone(),
                status: agent.agent_status,
                focused: agent.focused && source.active,
                text: grouped_agent_text(snapshot, agent),
            },
        }));
    }
}

/// `tab · agent · custom state label`, skipping parts that carry no information.
/// The status word itself is left to the colored indicator.
fn grouped_agent_text(
    snapshot: &ClientShellSnapshot,
    agent: &crate::protocol::ClientShellAgent,
) -> String {
    let tab = snapshot.tabs.iter().find(|tab| tab.tab_id == agent.tab_id);
    let custom_state = agent
        .state_labels
        .iter()
        .find(|(state, _)| state == status_text(agent.agent_status))
        .map(|(_, label)| label.as_str());
    [
        agent_tab_label(snapshot, agent, tab),
        agent_display_label(agent),
        custom_state,
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

/// Render grouped agent lines into `body`: one blank row separates spaces,
/// agents sit directly under their space header.
pub(super) fn render_grouped_agent_list(
    buffer: &mut Buffer,
    body: Rect,
    lines: &[GroupedAgentLine],
    empty_message: Option<&str>,
    config: &ClientShellConfig,
    agent_scroll: &mut usize,
    selected_workspace: Option<&WorkspaceNavigationTarget>,
    endpoint_hits: bool,
    hits: &mut ShellHitMap,
) {
    let row_heights = vec![1; lines.len()];
    let gaps = grouped_line_gaps(lines);
    render_agent_list_body(
        buffer,
        body,
        lines,
        &row_heights,
        &gaps,
        empty_message,
        config,
        agent_scroll,
        hits,
        |buffer, rect, line, hits| {
            render_grouped_agent_line(
                buffer,
                rect,
                line,
                config,
                selected_workspace,
                endpoint_hits,
                hits,
            );
        },
    );
}

pub(super) fn grouped_line_gaps(lines: &[GroupedAgentLine]) -> Vec<u16> {
    (0..lines.len())
        .map(|index| {
            u16::from(
                lines
                    .get(index + 1)
                    .is_some_and(|next| matches!(next.kind, GroupedAgentLineKind::Header { .. })),
            )
        })
        .collect()
}

fn render_grouped_agent_line(
    buffer: &mut Buffer,
    rect: Rect,
    line: &GroupedAgentLine,
    config: &ClientShellConfig,
    selected_workspace: Option<&WorkspaceNavigationTarget>,
    endpoint_hits: bool,
    hits: &mut ShellHitMap,
) {
    let palette = &config.palette;
    let width = rect.width as usize;
    match &line.kind {
        GroupedAgentLineKind::Header {
            workspace_id,
            label,
            agent_count,
            status,
            collapsed,
        } => {
            if selected_workspace
                .is_some_and(|target| target.matches(&line.endpoint_id, workspace_id))
            {
                buffer.set_style(
                    rect,
                    Style::default().bg(super::render::sidebar::workspace_selection_background(
                        palette,
                    )),
                );
            }
            let icon = status_icon(*status, config.status_indicators);
            let count = format!(" ({agent_count})");
            let reserved = 2 + display_width(&count) + 1 + display_width(icon);
            let spans = vec![
                Span::styled(
                    if *collapsed { "▸" } else { "▾" },
                    Style::default().fg(palette.accent),
                ),
                Span::raw(" "),
                Span::styled(
                    crate::ui::truncate_end(label, width.saturating_sub(reserved)),
                    Style::default()
                        .fg(palette.text)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(count, Style::default().fg(palette.overlay0)),
                Span::raw(" "),
                Span::styled(icon, Style::default().fg(status_color(*status, palette))),
            ];
            Paragraph::new(Line::from(spans)).render(rect, buffer);
            hits.agent_groups.push((
                rect,
                line.endpoint_id.clone(),
                agent_group_key(workspace_id),
            ));
        }
        GroupedAgentLineKind::Agent {
            pane_id,
            status,
            focused,
            text,
        } => {
            let color = status_color(*status, palette);
            let text_style = if *focused {
                Style::default().fg(color)
            } else {
                Style::default().fg(color).add_modifier(Modifier::DIM)
            };
            let spans = vec![
                Span::raw("  "),
                Span::styled(
                    status_icon(*status, config.status_indicators),
                    Style::default().fg(color),
                ),
                Span::raw(" "),
                Span::styled(
                    crate::ui::truncate_end(text, width.saturating_sub(4)),
                    text_style,
                ),
            ];
            let row_style = if *focused {
                Style::default().bg(palette.active_row_bg)
            } else {
                Style::default()
            };
            Paragraph::new(Line::from(spans))
                .style(row_style)
                .render(rect, buffer);
            if endpoint_hits {
                hits.endpoint_agents
                    .push((rect, line.endpoint_id.clone(), pane_id.clone()));
            } else {
                hits.agents.push((rect, pane_id.clone()));
            }
        }
    }
    if line.stale {
        buffer.set_style(
            rect,
            Style::default()
                .fg(palette.overlay0)
                .add_modifier(Modifier::DIM),
        );
    }
}

fn put_text(buffer: &mut Buffer, x: u16, y: u16, width: u16, text: &str, style: Style) {
    for (offset, character) in text.chars().take(width as usize).enumerate() {
        if let Some(cell) = buffer.cell_mut((x + offset as u16, y)) {
            cell.set_char(character).set_style(style);
        }
    }
}

fn display_width(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(text)
}

fn sidebar_status_text(status: crate::api::schema::AgentStatus) -> &'static str {
    use crate::api::schema::AgentStatus;
    match status {
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Working => "working",
        AgentStatus::Idle | AgentStatus::Unknown => "idle",
    }
}
